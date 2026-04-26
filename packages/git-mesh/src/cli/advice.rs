//! `git mesh advice` subcommand — session-scoped advice stream.

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::git::work_dir;

/// Allowed character set for `<sessionId>`, documented in error messages
/// and clap help. Path separators (`/`, `\`), NUL, and ASCII control
/// characters are forbidden so the id maps unambiguously to a single
/// per-session directory without collision rewrites.
const SESSION_ID_RULE: &str =
    "non-empty; ASCII letters, digits, `-`, `_`, and `.`; \
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
    pub session_id: String,

    #[command(subcommand)]
    pub command: Option<AdviceCommand>,

    /// Append per-reason documentation blocks to the render output.
    #[arg(long)]
    pub documentation: bool,
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Capture the current workspace tree into the file-backed session store.
    Snapshot,
    /// Record one or more read events in the file-backed session store.
    Read {
        /// Paths (optionally range-qualified) to record as reads.
        paths: Vec<String>,
    },
}

/// Top-level entry: dispatches to `snapshot`, `read`, or, when no
/// subcommand is given, runs the file-backed delta render pipeline.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    validate_session_id(&args.session_id)?;
    match args.command {
        Some(AdviceCommand::Snapshot) => run_advice_snapshot(repo, args.session_id),
        Some(AdviceCommand::Read { paths }) => run_advice_read(repo, args.session_id, paths),
        None => run_advice_render(repo, &args.session_id, args.documentation),
    }
}

/// Bare-render entry point: file-backed delta pipeline.
///
/// Implements parent §Phase 4 step list. Pre-stdout ordering of state
/// mutations is load-bearing for broken-pipe safety — see step 16.
fn run_advice_render(
    repo: &gix::Repository,
    session_id: &str,
    documentation: bool,
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
    let store = SessionStore::open(wd, &gd, session_id)?;

    // Step 2: require baseline.state — fail closed.
    if !store.dir().join("baseline.state").exists() {
        bail!(
            "no baseline for session `{session_id}`; run snapshot first \
             (`git mesh advice {session_id} snapshot`)"
        );
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
                    m.ranges.into_iter().map(move |r| MeshRange {
                        name: name.clone(),
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
                        status: match r.status {
                            crate::types::RangeStatus::Fresh => MeshRangeStatus::Stable,
                            crate::types::RangeStatus::Moved => MeshRangeStatus::Moved,
                            crate::types::RangeStatus::Changed => MeshRangeStatus::Changed,
                            _ => MeshRangeStatus::Terminal,
                        },
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
                let (s, e) = match add.extent {
                    crate::types::RangeExtent::Lines { start, end } => (start, end),
                    crate::types::RangeExtent::Whole => (0, u32::MAX),
                };
                staging_adds.push(StagedAddr {
                    path: std::path::PathBuf::from(add.path),
                    start: s,
                    end: e,
                });
            }
            for rem in staging.removes {
                let (s, e) = match rem.extent {
                    crate::types::RangeExtent::Lines { start, end } => (start, end),
                    crate::types::RangeExtent::Whole => (0, u32::MAX),
                };
                staging_removes.push(StagedAddr {
                    path: std::path::PathBuf::from(rem.path),
                    start: s,
                    end: e,
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
        staging: StagingState {
            adds: &staging_adds,
            removes: &staging_removes,
        },
    };
    let mut candidates: Vec<crate::advice::candidates::Candidate> = Vec::new();
    candidates.extend(crate::advice::candidates::detect_read_intersects_mesh(&input));
    candidates.extend(crate::advice::candidates::detect_delta_intersects_mesh(&input));
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
    use std::collections::BTreeSet;
    let mut emitted_topics: BTreeSet<String> = BTreeSet::new();
    for c in &kept {
        if let Some(topic) = c.reason_kind.doc_topic() {
            let key = topic.to_string();
            if !docs_seen.contains(&key) {
                emitted_topics.insert(key);
            }
        }
    }
    let new_doc_topics: Vec<String> = emitted_topics.into_iter().collect();
    let rendered = crate::advice::render::render(&kept, &new_doc_topics, documentation);

    // Build touch intervals (finding 3): one per affected path/range from
    // incr_delta + new_reads, sharing a single rfc3339 timestamp so a
    // future co-touch detector can group them by interval.
    let touch_ts = chrono::Utc::now().to_rfc3339();
    let touch_intervals_to_append: Vec<TouchInterval> =
        build_touch_intervals(incr_delta.as_slice(), &new_reads, &touch_ts);

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
        std::fs::remove_dir_all(&last_flush_objects).map_err(|e| {
            anyhow::anyhow!(
                "remove `{}`: {e}",
                last_flush_objects.display()
            )
        })?;
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
        handle.write_all(rendered.as_bytes()).and_then(|()| handle.flush())
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
) -> Vec<crate::advice::session::state::TouchInterval> {
    use crate::advice::session::state::TouchInterval;
    use crate::advice::workspace_tree::DiffEntry;
    let mut out: Vec<TouchInterval> = Vec::new();
    for entry in incr_delta {
        match entry {
            DiffEntry::Modified { path }
            | DiffEntry::Added { path }
            | DiffEntry::Deleted { path }
            | DiffEntry::ModeChange { path } => {
                out.push(TouchInterval {
                    path: path.clone(),
                    start_line: 0,
                    end_line: 0,
                    ts: ts.to_string(),
                });
            }
            DiffEntry::Renamed { from, to, .. } => {
                out.push(TouchInterval {
                    path: from.clone(),
                    start_line: 0,
                    end_line: 0,
                    ts: ts.to_string(),
                });
                out.push(TouchInterval {
                    path: to.clone(),
                    start_line: 0,
                    end_line: 0,
                    ts: ts.to_string(),
                });
            }
        }
    }
    for r in new_reads {
        out.push(TouchInterval {
            path: r.path.clone(),
            start_line: r.start_line.unwrap_or(0),
            end_line: r.end_line.unwrap_or(0),
            ts: ts.to_string(),
        });
    }
    out
}

/// Best-effort check that `tree_sha` resolves inside `objects_dir` (using
/// `git cat-file -e` against an alternate object directory). Returns false
/// on any failure, including when git isn't usable — the caller falls back
/// to baseline diff in that case. (Finding 2b.)
fn tree_resolves_in(
    _repo: &gix::Repository,
    tree_sha: &str,
    objects_dir: &std::path::Path,
) -> bool {
    let out = std::process::Command::new("git")
        .env("GIT_OBJECT_DIRECTORY", objects_dir)
        .args(["cat-file", "-e", tree_sha])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(out, Ok(s) if s.success())
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
    std::fs::create_dir_all(dst)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| anyhow::anyhow!("read_dir `{}`: {e}", src.display()))?
    {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| anyhow::anyhow!("copy `{}` -> `{}`: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Capture the current workspace tree into the file-backed session store.
fn run_advice_snapshot(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::BaselineState;
    use crate::advice::workspace_tree;

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let mut store = SessionStore::open(wd, &gd, &session_id)?;

    // Reset the JSONLs and any prior *.objects/.
    store.reset()?;

    // Capture into baseline.objects/.
    let baseline_objects = store.baseline_objects_dir();
    std::fs::create_dir_all(&baseline_objects)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", baseline_objects.display()))?;
    let tree = workspace_tree::capture(repo, &baseline_objects)?;

    // Compute index_sha (last 20 bytes of real index, hex).
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
    store.write_baseline(&state)?;

    // Mirror baseline -> last-flush.
    let last_flush_objects = store.last_flush_objects_dir();
    if last_flush_objects.exists() {
        std::fs::remove_dir_all(&last_flush_objects).ok();
    }
    copy_dir_recursive(&baseline_objects, &last_flush_objects)?;
    store.write_last_flush(&state)?;

    Ok(0)
}

/// Record read events in the file-backed session store.
fn run_advice_read(
    repo: &gix::Repository,
    session_id: String,
    paths: Vec<String>,
) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::ReadRecord;
    use crate::advice::session::store::LockTimeout;

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;

    // Require baseline.state — fail closed.
    if !store.dir().join("baseline.state").exists() {
        bail!(
            "no baseline for session `{session_id}`; run snapshot first \
             (`git mesh advice {session_id} snapshot`)"
        );
    }

    if paths.is_empty() {
        bail!("git mesh advice <id> read: at least one path is required");
    }

    // Validate every path/range first; only append if all are valid.
    for spec in &paths {
        validate_read_spec(repo, spec)?;
    }

    let now = chrono::Utc::now().to_rfc3339();
    for spec in &paths {
        let (path_str, range) = match spec.split_once("#L") {
            Some((p, frag)) => {
                let (s, e) = frag.split_once("-L").unwrap();
                (p.to_string(), Some((s.parse::<u32>().unwrap(), e.parse::<u32>().unwrap())))
            }
            None => (spec.clone(), None),
        };
        let rec = ReadRecord {
            path: path_str,
            start_line: range.map(|(s, _)| s),
            end_line: range.map(|(_, e)| e),
            ts: now.clone(),
        };
        store.append_read(&rec, LockTimeout::Bounded(std::time::Duration::from_secs(30)))?;
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
                anyhow::anyhow!(
                    "invalid range `{spec}`; expected <path>#L<start>-L<end>"
                )
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
        let bytes = std::fs::read(&abs)
            .map_err(|e| anyhow::anyhow!("read `{path_str}`: {e}"))?;
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
