//! `git mesh advice` subcommand — session-scoped stream that surfaces
//! the implicit semantic dependencies a developer has crossed.
//!
//! Each render emits one candidate per coupling crossed since the last
//! flush: a mesh range was read, edited, or deleted; a partner of an
//! edited range drifted; a rename broke an anchored path; sibling
//! ranges were touched in the same session; staging cuts across the
//! mesh. Candidates carry the mesh's `why` — the one-sentence
//! definition of the relationship the anchored ranges hold — so the
//! developer reads what the coupling is at the moment they're
//! stepping on it.

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::git::work_dir;

/// Allowed character set for `<sessionId>`, documented in error messages
/// and clap help. Path separators (`/`, `\`), NUL, and ASCII control
/// characters are forbidden so the id maps unambiguously to a single
/// per-session directory without collision rewrites.
const SESSION_ID_RULE: &str = "non-empty; ASCII letters, digits, `-`, `_`, and `.`; \
     no `/`, no `\\`, no NUL, no whitespace or other control characters";

#[derive(Debug, clap::Args)]
pub struct AdviceArgs {
    /// Session identifier (used to isolate per-session state).
    ///
    /// Allowed characters: ASCII letters, digits, `-`, `_`, and `.`.
    /// Path separators (`/`, `\`), NUL, whitespace, and other control
    /// characters are rejected — the id becomes a directory component
    /// under the per-session state directory and silent rewrites would
    /// collide distinct ids onto the same store.
    ///
    /// Not required when the subcommand is `suggest` — that subcommand
    /// is corpus-wide and loads every session from `GIT_MESH_ADVICE_DIR`.
    pub session_id: Option<String>,

    #[command(subcommand)]
    pub command: Option<AdviceCommand>,

    /// Append per-reason documentation blocks to the render. Blocks emit
    /// only when the flush surfaces a reason with an associated doc topic
    /// (rename, range shrink, symbol rename, cross-mesh overlap, terminal
    /// state, etc.); pure partner-read surfacings have no topic and
    /// produce no extra output. Each topic emits once per session —
    /// already-seen topics in `docs-seen.jsonl` are suppressed.
    #[arg(long)]
    pub documentation: bool,

    /// When `baseline.state` is absent, automatically take a snapshot before
    /// proceeding. A present-but-corrupt or unreadable baseline still fails
    /// closed — only the missing case triggers auto-bootstrap.
    #[arg(long)]
    pub snapshot_if_missing: bool,
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Capture the current workspace tree as the session baseline; later
    /// renders diff against it to detect dependency-crossing edits.
    Snapshot,
    /// Record one or more read events; later renders use these to surface
    /// dependencies the developer crossed by reading (not just editing).
    Read {
        /// Paths (optionally range-qualified) to record as reads.
        paths: Vec<String>,
    },
    /// Run the n-ary mesh suggestion detector against every session under
    /// `GIT_MESH_ADVICE_DIR` and emit each suggestion as one `{"v":1, …}`
    /// JSON line on stdout. Hidden because this is the parity surface for
    /// diffing against `node docs/analyze-v4.mjs` during development —
    /// the user-facing advice flow is the default `git mesh advice <sid>`.
    ///
    /// Env vars:
    ///   GIT_MESH_ADVICE_DIR            Required. Root directory of captured sessions.
    ///   GIT_MESH_SUGGEST_TRIGRAM=0     Disable trigram cohesion (mirrors JS --no-trigram).
    ///   GIT_MESH_SUGGEST_HISTORY=0     Disable historical-cochange channel (mirrors JS --no-history).
    ///   GIT_MESH_SUGGEST_FIXTURE=1     Disable history channel for fixture/parity runs (prevents repo contamination).
    ///   GIT_MESH_SUGGEST_REPO_ROOT     Override repo root for cohesion file reads (default: cwd).
    #[command(hide = true)]
    Suggest,
}

/// Top-level entry: dispatches to `snapshot`, `read`, `suggest`, or, when no
/// subcommand is given, runs the file-backed delta render pipeline.
///
/// `Suggest` does not require a `<SESSION_ID>` — it is corpus-wide and reads
/// every session from `GIT_MESH_ADVICE_DIR`. All other variants require a
/// session id and fail closed when one is not provided.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    // Suggest is corpus-wide — no session id required.
    if matches!(args.command, Some(AdviceCommand::Suggest)) {
        return run_advice_suggest();
    }
    // All other subcommands (and the bare render) are session-scoped.
    let session_id = args.session_id.ok_or_else(|| {
        anyhow::anyhow!(
            "git mesh advice: a <SESSION_ID> is required (e.g. `git mesh advice <id>`)"
        )
    })?;
    validate_session_id(&session_id)?;
    match args.command {
        Some(AdviceCommand::Snapshot) => run_advice_snapshot(repo, session_id),
        Some(AdviceCommand::Read { paths }) => {
            run_advice_read(repo, session_id, paths, args.snapshot_if_missing)
        }
        Some(AdviceCommand::Suggest) => unreachable!("handled above"),
        None => run_advice_render(
            repo,
            &session_id,
            args.documentation,
            args.snapshot_if_missing,
        ),
    }
}

/// Bare-render entry point: file-backed delta pipeline. Walks the
/// session delta, the incremental delta since last flush, and recorded
/// reads against the mesh state, and emits one candidate per implicit
/// semantic dependency the developer has crossed but not yet seen
/// advice for.
///
/// Implements parent §Phase 4 step list. Pre-stdout ordering of state
/// mutations is load-bearing for broken-pipe safety — see step 16.
fn run_advice_render(
    repo: &gix::Repository,
    session_id: &str,
    documentation: bool,
    snapshot_if_missing: bool,
) -> Result<i32> {
    use crate::advice::candidates::{
        CandidateInput, MeshRange, MeshRangeStatus, StagedAddr, StagingState,
    };
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::TouchInterval;
    use crate::advice::workspace_tree;

    /// RAII guard that removes a directory if the path still exists at drop
    /// time. The success path calls `.disarm()` before renaming the directory
    /// out from under the guard. (Finding 6.)
    struct DirGuard {
        path: Option<std::path::PathBuf>,
    }
    impl DirGuard {
        fn new(path: std::path::PathBuf) -> Self {
            Self { path: Some(path) }
        }
        fn disarm(mut self) {
            self.path = None;
        }
    }
    impl Drop for DirGuard {
        fn drop(&mut self) {
            if let Some(p) = self.path.take()
                && p.exists()
            {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let mut store = SessionStore::open(wd, &gd, session_id)?;
    let internal_path_prefixes = active_advice_store_prefixes(wd, store.dir());

    // Step 2: require baseline.state — fail closed unless --snapshot-if-missing.
    if !store.dir().join("baseline.state").exists() {
        if snapshot_if_missing {
            snapshot_into(&mut store, repo, &gd)?;
        } else {
            bail!(
                "no baseline for session `{session_id}`; run snapshot first \
                 (`git mesh advice {session_id} snapshot`)"
            );
        }
    }
    let baseline = store.read_baseline()?;

    // Step 3: capture current workspace tree into current.objects-<uuid>/.
    let cur_uuid = uuid::Uuid::new_v4();
    let current_objects = store.dir().join(format!("current.objects-{cur_uuid}"));
    std::fs::create_dir_all(&current_objects)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", current_objects.display()))?;
    // Guard the temp objects dir so any `?` between here and the rename
    // below reclaims the directory on drop. (Finding 6.)
    let current_objects_guard = DirGuard::new(current_objects.clone());
    let current = workspace_tree::capture(repo, &current_objects)?;

    // Step 4: diff_trees(baseline → current).
    let baseline_objects = store.baseline_objects_dir();
    let session_delta = workspace_tree::diff_trees(
        repo,
        &baseline.tree_sha,
        &current.tree_sha,
        &baseline_objects,
        &current_objects,
    )?;

    // Step 5: read last-flush.state (if absent OR inconsistent with the
    // contents of last-flush.objects/, fall back to baseline — see
    // finding 2b: a crash between the rename and the state write leaves a
    // stale state pointing at a tree that's no longer in objects/).
    let last_flush_state_path = store.dir().join("last-flush.state");
    let last_flush_objects_path = store.last_flush_objects_dir();
    let (last_flush_state, last_flush_objects_for_diff) = if last_flush_state_path.exists() {
        let st = store.read_last_flush()?;
        let consistent = last_flush_objects_path.exists()
            && tree_resolves_in(repo, &st.tree_sha, &last_flush_objects_path);
        if consistent {
            (st, last_flush_objects_path.clone())
        } else {
            eprintln!(
                "git mesh advice: last-flush state inconsistent with last-flush.objects; falling back to baseline diff"
            );
            (baseline.clone(), baseline_objects.clone())
        }
    } else {
        (baseline.clone(), baseline_objects.clone())
    };

    // Step 6: diff_trees(last_flush → current).
    let last_flush_objects = last_flush_objects_path;
    let incr_delta = workspace_tree::diff_trees(
        repo,
        &last_flush_state.tree_sha,
        &current.tree_sha,
        &last_flush_objects_for_diff,
        &current_objects,
    )?;

    // Step 7: reads_since_cursor — cursor lives in last-flush.state.
    let read_cursor = last_flush_state.read_cursor;
    let new_reads = store.reads_since_cursor(read_cursor)?;

    // Step 8: load mesh state. Treat any error as empty (greenfield: a
    // missing mesh-state directory is not a render failure).
    let mesh_ranges: Vec<MeshRange> =
        match crate::resolver::stale_meshes(repo, default_engine_options()) {
            Ok(meshes) => meshes
                .into_iter()
                .flat_map(|m| {
                    let name = m.name.clone();
                    let why = m.message.clone();
                    m.ranges.into_iter().map(move |r| {
                        let whole = matches!(r.anchored.extent, crate::types::RangeExtent::Whole);
                        MeshRange {
                            name: name.clone(),
                            why: why.clone(),
                            path: std::path::PathBuf::from(
                                r.anchored.path.to_string_lossy().into_owned(),
                            ),
                            start: match r.anchored.extent {
                                crate::types::RangeExtent::Lines { start, .. } => start,
                                crate::types::RangeExtent::Whole => 0,
                            },
                            end: match r.anchored.extent {
                                crate::types::RangeExtent::Lines { end, .. } => end,
                                crate::types::RangeExtent::Whole => u32::MAX,
                            },
                            whole,
                            status: match r.status {
                                crate::types::RangeStatus::Fresh => MeshRangeStatus::Stable,
                                crate::types::RangeStatus::Moved => MeshRangeStatus::Moved,
                                crate::types::RangeStatus::Changed => MeshRangeStatus::Changed,
                                _ => MeshRangeStatus::Terminal,
                            },
                        }
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        };

    // Step 9: load staging from .git/mesh/staging across staged mesh names.
    let mut staging_adds: Vec<StagedAddr> = Vec::new();
    let mut staging_removes: Vec<StagedAddr> = Vec::new();
    if let Ok(names) = crate::staging::list_staged_mesh_names(repo) {
        for name in names {
            let Ok(staging) = crate::staging::read_staging(repo, &name) else {
                continue;
            };
            for add in staging.adds {
                let whole = matches!(add.extent, crate::types::RangeExtent::Whole);
                let (s, e) = match add.extent {
                    crate::types::RangeExtent::Lines { start, end } => (start, end),
                    crate::types::RangeExtent::Whole => (0, u32::MAX),
                };
                staging_adds.push(StagedAddr {
                    path: std::path::PathBuf::from(add.path),
                    start: s,
                    end: e,
                    whole,
                });
            }
            for rem in staging.removes {
                let whole = matches!(rem.extent, crate::types::RangeExtent::Whole);
                let (s, e) = match rem.extent {
                    crate::types::RangeExtent::Lines { start, end } => (start, end),
                    crate::types::RangeExtent::Whole => (0, u32::MAX),
                };
                staging_removes.push(StagedAddr {
                    path: std::path::PathBuf::from(rem.path),
                    start: s,
                    end: e,
                    whole,
                });
            }
        }
    }

    // Step 10–12: load seen sets and historical touch intervals.
    let advice_seen = store.advice_seen_set()?;
    let docs_seen = store.docs_seen_set()?;
    let touch_intervals = store.all_touch_intervals()?;

    // Step 13: produce candidates.
    let input = CandidateInput {
        session_delta: &session_delta,
        incr_delta: &incr_delta,
        new_reads: &new_reads,
        touch_intervals: &touch_intervals,
        mesh_ranges: &mesh_ranges,
        internal_path_prefixes: &internal_path_prefixes,
        staging: StagingState {
            adds: &staging_adds,
            removes: &staging_removes,
        },
    };
    let mut candidates: Vec<crate::advice::candidates::Candidate> = Vec::new();
    candidates.extend(crate::advice::candidates::detect_read_intersects_mesh(
        &input,
    ));
    candidates.extend(crate::advice::candidates::detect_delta_intersects_mesh(
        &input,
    ));
    candidates.extend(crate::advice::candidates::detect_partner_drift(&input));
    candidates.extend(crate::advice::candidates::detect_rename_consequence(&input));
    candidates.extend(crate::advice::candidates::detect_range_shrink(&input));
    candidates.extend(crate::advice::candidates::detect_session_co_touch(&input));
    candidates.extend(crate::advice::candidates::detect_staging_cross_cut(&input));

    // Step 14: filter out fingerprints in advice_seen_set.
    let mut emitted_fps: Vec<String> = Vec::new();
    let kept: Vec<crate::advice::candidates::Candidate> = candidates
        .into_iter()
        .filter_map(|c| {
            let fp = crate::advice::fingerprint::fingerprint(&c);
            if advice_seen.contains(&fp) {
                None
            } else {
                emitted_fps.push(fp);
                Some(c)
            }
        })
        .collect();

    // Step 15: render. Compute new_doc_topics = topic_keys(emitted_reason_kinds) - docs_seen.
    // Only populated when --documentation is requested; bare renders must not
    // record topic keys in docs-seen.jsonl (Bug 3).
    use std::collections::BTreeSet;
    let new_doc_topics: Vec<String> = if documentation {
        let mut emitted_topics: BTreeSet<String> = BTreeSet::new();
        for c in &kept {
            if let Some(topic) = c.reason_kind.doc_topic() {
                let key = topic.to_string();
                if !docs_seen.contains(&key) {
                    emitted_topics.insert(key);
                }
            }
        }
        emitted_topics.into_iter().collect()
    } else {
        Vec::new()
    };
    // Convert candidates to Suggestions at the Detector seam before rendering.
    let kept_suggestions: Vec<crate::advice::suggestion::Suggestion> = kept
        .iter()
        .map(crate::advice::candidates::candidate_to_suggestion)
        .collect();
    let rendered = crate::advice::render::render(&kept_suggestions, &new_doc_topics, documentation);

    // Build touch intervals (finding 3): one per affected path/range from
    // incr_delta + new_reads, sharing a single rfc3339 timestamp so a
    // future co-touch detector can group them by interval.
    let touch_ts = chrono::Utc::now().to_rfc3339();
    let touch_intervals_to_append: Vec<TouchInterval> = build_touch_intervals(
        incr_delta.as_slice(),
        &new_reads,
        &touch_ts,
        &internal_path_prefixes,
    );

    // Step 16 (revised — finding 1 + finding 2a):
    //
    // 1. Cache-correctness invariants (rename last-flush.objects, write
    //    last-flush.state with the new read_cursor) ALWAYS run before
    //    stdout. They survive an EPIPE so the next render diffs against
    //    the tree we just rendered against, not against a stale one.
    // 2. Seen-set / touches.jsonl appends ONLY run on stdout success or
    //    EPIPE. Any other stdout error returns Err without advancing
    //    those sets, so candidates resurface on the next render.

    // d) Promote current.objects-<uuid>/ to last-flush.objects/. From
    // here on the guard must NOT remove the directory.
    if last_flush_objects.exists() {
        std::fs::remove_dir_all(&last_flush_objects)
            .map_err(|e| anyhow::anyhow!("remove `{}`: {e}", last_flush_objects.display()))?;
    }
    std::fs::rename(&current_objects, &last_flush_objects).map_err(|e| {
        anyhow::anyhow!(
            "rename `{}` -> `{}`: {e}",
            current_objects.display(),
            last_flush_objects.display()
        )
    })?;
    current_objects_guard.disarm();

    // e) Write last-flush.state with the consolidated read_cursor (2a).
    // SAFETY: session lock held (acquired in SessionStore::open and kept for
    // the command lifetime); concurrent `git mesh advice <id> read` calls
    // block on the same flock, so no new ReadRecord can be appended between
    // the reads_since_cursor call above and reads_byte_len here.
    let new_cursor = store.reads_byte_len()?;
    let new_last_flush = crate::advice::session::state::BaselineState {
        schema_version: crate::advice::session::SCHEMA_VERSION,
        tree_sha: current.tree_sha.clone(),
        index_sha: baseline.index_sha.clone(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        read_cursor: new_cursor,
    };
    store.write_last_flush(&new_last_flush)?;

    // Step 17: write rendered output, then on success/EPIPE advance the
    // observation sets (advice-seen, docs-seen, touches.jsonl).
    use std::io::Write;
    let stdout_result = if rendered.is_empty() {
        Ok(())
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(rendered.as_bytes())
            .and_then(|()| handle.flush())
    };
    match stdout_result {
        Ok(()) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            // intentional: the pipe peer left; treat as observed.
        }
        Err(e) => {
            // Non-EPIPE write failure: do NOT advance seen sets, so the
            // candidates resurface on the next render. (Finding 1.)
            return Err(anyhow::Error::from(e).context("write advice to stdout"));
        }
    }

    if !emitted_fps.is_empty() {
        store.append_advice_seen(&emitted_fps)?;
    }
    if !new_doc_topics.is_empty() {
        store.append_docs_seen(&new_doc_topics)?;
    }
    for interval in &touch_intervals_to_append {
        store.append_touch(interval)?;
    }

    Ok(0)
}

/// Translate a session's `incr_delta` + `new_reads` into one
/// `TouchInterval` per affected path/range, all sharing `ts` so a co-touch
/// detector can group them by timestamp. (Finding 3.)
fn build_touch_intervals(
    incr_delta: &[crate::advice::workspace_tree::DiffEntry],
    new_reads: &[crate::advice::session::state::ReadRecord],
    ts: &str,
    internal_path_prefixes: &[String],
) -> Vec<crate::advice::session::state::TouchInterval> {
    use crate::advice::session::state::TouchInterval;
    use crate::advice::workspace_tree::DiffEntry;
    let mut out: Vec<TouchInterval> = Vec::new();
    for entry in incr_delta {
        match entry {
            DiffEntry::Modified { path, .. }
            | DiffEntry::Added { path, .. }
            | DiffEntry::Deleted { path, .. }
            | DiffEntry::ModeChange { path, .. } => {
                if advice_path_is_internal(path, internal_path_prefixes) {
                    continue;
                }
                out.push(TouchInterval {
                    path: path.clone(),
                    start_line: 0,
                    end_line: 0,
                    ts: ts.to_string(),
                });
            }
            DiffEntry::Renamed { from, to, .. } => {
                if !advice_path_is_internal(from, internal_path_prefixes) {
                    out.push(TouchInterval {
                        path: from.clone(),
                        start_line: 0,
                        end_line: 0,
                        ts: ts.to_string(),
                    });
                }
                if !advice_path_is_internal(to, internal_path_prefixes) {
                    out.push(TouchInterval {
                        path: to.clone(),
                        start_line: 0,
                        end_line: 0,
                        ts: ts.to_string(),
                    });
                }
            }
        }
    }
    for r in new_reads {
        if advice_path_is_internal(&r.path, internal_path_prefixes) {
            continue;
        }
        out.push(TouchInterval {
            path: r.path.clone(),
            start_line: r.start_line.unwrap_or(0),
            end_line: r.end_line.unwrap_or(0),
            ts: ts.to_string(),
        });
    }
    out
}

fn active_advice_store_prefixes(
    repo_root: &std::path::Path,
    store_dir: &std::path::Path,
) -> Vec<String> {
    let repo_root = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let store_dir = std::fs::canonicalize(store_dir).unwrap_or_else(|_| store_dir.to_path_buf());
    let Ok(rel) = store_dir.strip_prefix(&repo_root) else {
        return Vec::new();
    };
    if rel.as_os_str().is_empty() {
        return Vec::new();
    }
    vec![
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    ]
}

fn advice_path_is_internal(path: &str, internal_path_prefixes: &[String]) -> bool {
    internal_path_prefixes.iter().any(|prefix| {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Best-effort check that `tree_sha` resolves inside `objects_dir` (using
/// `git cat-file -e` against an alternate object directory). Returns false
/// on any failure, including when git isn't usable — the caller falls back
/// to baseline diff in that case. (Finding 2b.)
fn tree_resolves_in(repo: &gix::Repository, tree_sha: &str, objects_dir: &std::path::Path) -> bool {
    // The captured tree can be either session-owned (untracked or edited
    // workspace content) or already present in the real repository object db
    // as a loose object (a clean snapshot often reuses HEAD's tree). Accept
    // both stores, but do not let Git follow `.git/objects/info/alternates`;
    // an alternate pool can otherwise hide a missing session object store.
    let repo_path = repo.path().parent().unwrap_or(repo.path());
    let out = std::process::Command::new("git")
        .current_dir(repo_path)
        .env("GIT_OBJECT_DIRECTORY", objects_dir)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
        .args(["cat-file", "-e", tree_sha])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(out, Ok(s) if s.success()) {
        return true;
    }
    real_loose_object_exists(repo, tree_sha)
}

fn real_loose_object_exists(repo: &gix::Repository, oid: &str) -> bool {
    if oid.len() < 3 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let (dir, file) = oid.split_at(2);
    repo.git_dir().join("objects").join(dir).join(file).is_file()
}

fn default_engine_options() -> crate::types::EngineOptions {
    crate::types::EngineOptions {
        layers: crate::types::LayerSet {
            worktree: true,
            index: true,
            staged_mesh: true,
        },
        ignore_unavailable: false,
        since: None,
    }
}

/// Recursively copy directory contents from `src` to `dst`.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", dst.display()))?;
    for entry in
        std::fs::read_dir(src).map_err(|e| anyhow::anyhow!("read_dir `{}`: {e}", src.display()))?
    {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to).map_err(|e| {
                anyhow::anyhow!("copy `{}` -> `{}`: {e}", from.display(), to.display())
            })?;
        }
    }
    Ok(())
}

/// Perform a baseline snapshot into an already-open `store`. Resets the
/// JSONLs and *.objects dirs, captures the current workspace tree, writes
/// `baseline.state`, and mirrors it to `last-flush.state`. The caller must
/// hold the store's lock — do NOT reopen the store here to avoid a second
/// flock on the same directory.
fn snapshot_into(
    store: &mut crate::advice::session::SessionStore,
    repo: &gix::Repository,
    gd: &std::path::Path,
) -> Result<()> {
    use crate::advice::session::state::BaselineState;
    use crate::advice::workspace_tree;

    store.reset()?;

    let baseline_objects = store.baseline_objects_dir();
    std::fs::create_dir_all(&baseline_objects)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", baseline_objects.display()))?;
    let tree = workspace_tree::capture(repo, &baseline_objects)?;

    let index_path = gd.join("index");
    let index_sha = if let Ok(bytes) = std::fs::read(&index_path) {
        if bytes.len() >= 20 {
            let tail = &bytes[bytes.len() - 20..];
            let mut s = String::with_capacity(40);
            for b in tail {
                s.push_str(&format!("{b:02x}"));
            }
            s
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let captured_at = chrono::Utc::now().to_rfc3339();
    let state = BaselineState {
        schema_version: crate::advice::session::SCHEMA_VERSION,
        tree_sha: tree.tree_sha.clone(),
        index_sha,
        captured_at,
        read_cursor: 0,
    };
    let last_flush_objects = store.last_flush_objects_dir();
    if last_flush_objects.exists() {
        std::fs::remove_dir_all(&last_flush_objects).ok();
    }
    copy_dir_recursive(&baseline_objects, &last_flush_objects)?;
    store.write_last_flush(&state)?;
    // `baseline.state` is written last so its presence witnesses last-flush durability.
    store.write_baseline(&state)?;

    Ok(())
}

/// Standalone entry point for `git mesh advice suggest`, callable before repo
/// discovery. Used by `main.rs` to bypass the require-repo gate for this
/// hidden subcommand.
pub fn run_advice_suggest_standalone() -> Result<i32> {
    run_advice_suggest()
}

/// Run the n-ary mesh suggestion detector against every session under
/// `GIT_MESH_ADVICE_DIR` and emit each suggestion as one JSON line to stdout.
///
/// Sessions are discovered by scanning every subdirectory of
/// `GIT_MESH_ADVICE_DIR` that contains `reads.jsonl` and/or `touches.jsonl`.
/// A git repository is opened from the current directory for the history
/// channel; if none is found, or if `GIT_MESH_SUGGEST_FIXTURE=1` is set,
/// history is disabled.
fn run_advice_suggest() -> Result<i32> {
    use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};

    // Fail closed: GIT_MESH_ADVICE_DIR must be set and non-empty.
    let advice_dir_str = std::env::var("GIT_MESH_ADVICE_DIR").unwrap_or_default();
    if advice_dir_str.is_empty() {
        bail!(
            "GIT_MESH_ADVICE_DIR is not set; the suggester is the parity surface \
             and requires a captured session corpus"
        );
    }
    let advice_dir = std::path::PathBuf::from(&advice_dir_str);

    // Fail closed: the directory must exist.
    if !advice_dir.exists() {
        bail!(
            "GIT_MESH_ADVICE_DIR points at a directory that does not exist: `{}`",
            advice_dir.display()
        );
    }

    let cfg = SuggestConfig::from_env();

    // The pipeline loads ALL sessions from GIT_MESH_ADVICE_DIR. Each
    // subdirectory with reads.jsonl and/or touches.jsonl is one session.
    let sessions = load_all_sessions(&advice_dir)?;

    // Fail closed: at least one valid session must exist.
    if sessions.is_empty() {
        bail!(
            "no sessions found under `{}`; a session directory must contain \
             reads.jsonl or touches.jsonl",
            advice_dir.display()
        );
    }

    // Attempt to open the git repo for the history channel.
    // History is disabled when:
    //   - GIT_MESH_SUGGEST_HISTORY=0 (explicit user knob), OR
    //   - GIT_MESH_SUGGEST_FIXTURE=1 (parity/fixture run — prevents repo contamination)
    // Using GIT_MESH_ADVICE_DIR to gate history has been removed: a custom advice
    // directory is a valid user configuration and must not silently degrade quality.
    let fixture_mode = std::env::var("GIT_MESH_SUGGEST_FIXTURE").as_deref() == Ok("1");
    if fixture_mode {
        eprintln!("fixture mode: GIT_MESH_SUGGEST_FIXTURE is set; historical-cochange channel disabled");
    }
    let (repo_opt, repo_root) = if cfg.history_enabled && !fixture_mode {
        match gix::discover(".") {
            Ok(repo) => {
                let root = repo
                    .workdir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                (Some(repo), root)
            }
            Err(_) => (None, std::path::PathBuf::from(".")),
        }
    } else {
        (None, std::path::PathBuf::from("."))
    };

    // Allow parity-test harnesses to point the cohesion stage at a fixture
    // directory containing source files, independent of the working directory.
    let repo_root = if let Ok(override_root) = std::env::var("GIT_MESH_SUGGEST_REPO_ROOT") {
        if !override_root.is_empty() {
            std::path::PathBuf::from(override_root)
        } else {
            repo_root
        }
    } else {
        repo_root
    };

    let suggestions = run_suggest_pipeline(
        &sessions,
        repo_opt.as_ref(),
        &repo_root,
        &cfg,
    );

    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for s in &suggestions {
        let line = serde_json::to_string(s)
            .map_err(|e| anyhow::anyhow!("serialize suggestion: {e}"))?;
        writeln!(handle, "{line}")
            .map_err(|e| anyhow::anyhow!("write suggestion: {e}"))?;
    }
    Ok(0)
}

/// Load JSONL lines from a file, returning an empty vec if the file is absent.
fn load_jsonl_lines<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<Vec<T>> {
    use std::io::BufRead;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::anyhow!("open `{}`: {e}", path.display())),
    };
    let mut out = Vec::new();
    for (idx, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| anyhow::anyhow!("read `{}`: {e}", path.display()))?;
        if line.is_empty() {
            continue;
        }
        let v: T = serde_json::from_str(&line).map_err(|e| {
            anyhow::anyhow!("parse `{}` line {}: {e}", path.display(), idx + 1)
        })?;
        out.push(v);
    }
    Ok(out)
}

/// Load all sessions from a directory by scanning subdirectories.
///
/// Each subdirectory that contains `reads.jsonl` and/or `touches.jsonl`
/// is treated as one session. Sessions are sorted by directory name for
/// deterministic ordering.
fn load_all_sessions(dir: &std::path::Path) -> Result<Vec<crate::advice::suggest::SessionRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    // Compute the current repo key so we can prefer sessions for this repo.
    // If we are not in a repo (e.g. fixture mode), `preferred_key` is None
    // and all sessions are accepted.
    let preferred_key: Option<String> = gix::discover(".")
        .ok()
        .and_then(|repo| {
            let root = repo.workdir().map(|p| p.to_path_buf())?;
            let git_dir = repo.git_dir().to_path_buf();
            Some(crate::advice::session::store::repo_key(&root, &git_dir))
        });

    /// Load all session directories found directly under `session_root`.
    fn load_session_dirs(
        session_root: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>> {
        if !session_root.exists() {
            return Ok(Vec::new());
        }
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(session_root)
            .map_err(|e| anyhow::anyhow!("read_dir `{}`: {e}", session_root.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
            .map(|e| e.path())
            .filter(|p| p.join("reads.jsonl").exists() || p.join("touches.jsonl").exists())
            .collect();
        dirs.sort();
        Ok(dirs)
    }

    fn sessions_from_dirs(
        dirs: Vec<std::path::PathBuf>,
    ) -> Result<Vec<crate::advice::suggest::SessionRecord>> {
        use crate::advice::session::state::{ReadRecord, TouchInterval};
        use crate::advice::suggest::SessionRecord;
        let mut sessions = Vec::new();
        for entry in dirs {
            let reads_path = entry.join("reads.jsonl");
            let touches_path = entry.join("touches.jsonl");
            let sid = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if sid.is_empty() {
                continue;
            }
            let reads = load_jsonl_lines::<ReadRecord>(&reads_path)?;
            let touches = load_jsonl_lines::<TouchInterval>(&touches_path)?;
            sessions.push(SessionRecord { sid, reads, touches });
        }
        Ok(sessions)
    }

    // Walk the first level of `dir`.
    // For each subdirectory:
    //   - If it directly contains reads.jsonl or touches.jsonl → flat/fixture layout
    //   - Otherwise → two-level real layout; descend one more level
    let mut flat_dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut two_level_preferred: Vec<std::path::PathBuf> = Vec::new();
    let mut two_level_other: Vec<std::path::PathBuf> = Vec::new();

    let first_level: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read_dir `{}`: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|e| e.path())
        .collect();

    for entry in first_level {
        let has_session_files = entry.join("reads.jsonl").exists()
            || entry.join("touches.jsonl").exists();
        if has_session_files {
            // Flat/fixture layout: this entry itself is a session dir.
            flat_dirs.push(entry);
        } else {
            // Two-level layout: this entry is a repo_key directory.
            let key = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let is_preferred = preferred_key.as_deref() == Some(key.as_str());
            let sub_dirs = load_session_dirs(&entry)?;
            if is_preferred {
                two_level_preferred.extend(sub_dirs);
            } else {
                two_level_other.extend(sub_dirs);
            }
        }
    }

    // Prefer: flat fixture dirs + preferred repo sessions.
    // If we are in a repo but no sessions exist for it, fall back to all two-level sessions
    // so the corpus is still usable (e.g. in fixture/test scenarios with arbitrary keys).
    let mut chosen_dirs = flat_dirs;
    if !two_level_preferred.is_empty() {
        // Preferred repo has sessions — use only those (plus any flat dirs above).
        chosen_dirs.extend(two_level_preferred);
    } else {
        // No preferred sessions: accept all two-level sessions regardless of repo key.
        chosen_dirs.extend(two_level_other);
    }
    chosen_dirs.sort();

    sessions_from_dirs(chosen_dirs)
}

/// Capture the current workspace tree into the file-backed session store.
fn run_advice_snapshot(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let mut store = SessionStore::open(wd, &gd, &session_id)?;
    snapshot_into(&mut store, repo, &gd)?;
    Ok(0)
}

/// Record read events in the file-backed session store.
fn run_advice_read(
    repo: &gix::Repository,
    session_id: String,
    paths: Vec<String>,
    snapshot_if_missing: bool,
) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::ReadRecord;
    use crate::advice::session::store::LockTimeout;

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let mut store = SessionStore::open(wd, &gd, &session_id)?;

    if paths.is_empty() {
        bail!("git mesh advice <id> read: at least one path is required");
    }

    // Validate every path/range first; only append if all are valid.
    for spec in &paths {
        validate_read_spec(repo, spec)?;
    }

    // Require baseline.state — fail closed unless --snapshot-if-missing.
    if !store.dir().join("baseline.state").exists() {
        if snapshot_if_missing {
            snapshot_into(&mut store, repo, &gd)?;
        } else {
            bail!(
                "no baseline for session `{session_id}`; run snapshot first \
                 (`git mesh advice {session_id} snapshot`)"
            );
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    for spec in &paths {
        let (path_str, range) = match spec.split_once("#L") {
            Some((p, frag)) => {
                let (s, e) = frag.split_once("-L").unwrap();
                (
                    p.to_string(),
                    Some((s.parse::<u32>().unwrap(), e.parse::<u32>().unwrap())),
                )
            }
            None => (spec.clone(), None),
        };
        let rec = ReadRecord {
            path: path_str,
            start_line: range.map(|(s, _)| s),
            end_line: range.map(|(_, e)| e),
            ts: now.clone(),
        };
        store.append_read(
            &rec,
            LockTimeout::Bounded(std::time::Duration::from_secs(30)),
        )?;
    }

    Ok(0)
}

/// Reject session ids that would silently collide on disk or escape the
/// per-session directory. See `SESSION_ID_RULE`.
fn validate_session_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("invalid <sessionId>: must not be empty ({SESSION_ID_RULE})");
    }
    for ch in id.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !ok {
            bail!(
                "invalid <sessionId> `{id}`: contains disallowed character `{}` ({SESSION_ID_RULE})",
                ch.escape_debug()
            );
        }
    }
    if id == "." || id == ".." {
        bail!("invalid <sessionId> `{id}`: reserved path component ({SESSION_ID_RULE})");
    }
    Ok(())
}

/// Reject `read` specs that point at non-existent paths or out-of-range
/// / inverted line ranges.
fn validate_read_spec(repo: &gix::Repository, spec: &str) -> Result<()> {
    if spec.is_empty() {
        bail!("invalid spec: path must not be empty");
    }
    let (path_str, range) = match spec.split_once("#L") {
        Some((p, frag)) => {
            let (s, e) = frag.split_once("-L").ok_or_else(|| {
                anyhow::anyhow!("invalid range `{spec}`; expected <path>#L<start>-L<end>")
            })?;
            let start: u32 = s
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range start in `{spec}`"))?;
            let end: u32 = e
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range end in `{spec}`"))?;
            if start < 1 {
                bail!("invalid range `{spec}`: start must be at least 1");
            }
            if end < start {
                bail!("invalid range `{spec}`: end ({end}) is before start ({start})");
            }
            (p, Some((start, end)))
        }
        None => (spec, None),
    };
    if path_str.is_empty() {
        bail!("invalid spec `{spec}`: path must not be empty");
    }
    let wd = work_dir(repo)?;
    let abs = wd.join(path_str);
    if !abs.exists() {
        bail!("path not found in worktree: `{path_str}`");
    }
    if let Some((start, end)) = range {
        let bytes = std::fs::read(&abs).map_err(|e| anyhow::anyhow!("read `{path_str}`: {e}"))?;
        let line_count = String::from_utf8_lossy(&bytes).lines().count() as u32;
        if end > line_count {
            bail!(
                "invalid range `{spec}`: end ({end}) is past EOF (extent has {line_count} lines)"
            );
        }
        let _ = start;
    }
    Ok(())
}
