//! `git mesh advice` subcommand — session-scoped stream that surfaces
//! the implicit semantic dependencies a developer has crossed.
//!
//! Each render emits one candidate per coupling crossed since the last
//! flush: a mesh anchor was read, edited, or deleted; a partner of an
//! edited anchor drifted; a rename broke an anchored path; sibling
//! anchors were touched in the same session; staging cuts across the
//! mesh. Candidates carry the mesh's `why` — the one-sentence
//! definition of the relationship the anchored set holds — so the
//! developer reads what the coupling is at the moment they're
//! stepping on it.

#[cfg(test)]
mod tests;

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
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Capture the current workspace tree as the session baseline; later
    /// verbs diff against it to detect dependency-crossing edits.
    Snapshot,
    /// Record a single read event (anchor or whole-file path); later verbs
    /// use this to surface dependencies crossed by reading, not just editing.
    Read {
        /// Anchor to record as read. Either a plain repo-relative path
        /// (whole-file) or `<path>#L<start>-L<end>` (line-range anchor).
        anchor: String,
    },
    /// Flush the current session delta: emit advice for every implicit
    /// dependency crossed since the last milestone or snapshot.
    Milestone,
    /// Flush the session and emit a final reconciliation sweep for any
    /// touched-and-stale meshes not yet announced this session.
    Stop,
    /// Corpus-wide debug/parity surface for the n-ary mesh suggester:
    /// runs `run_suggest_pipeline` against every session under
    /// `GIT_MESH_ADVICE_DIR` and emits each suggestion as one
    /// `{"v":1, …}` JSON line on stdout.
    ///
    /// The same suggester now also feeds the per-session render
    /// (`git mesh advice <SESSION_ID>`); this subcommand exists for
    /// parity diffs against `node docs/analyze-v4.mjs` and for
    /// inspecting the unfiltered, cross-session output.
    ///
    /// Env vars:
    ///   GIT_MESH_ADVICE_DIR            Required. Root directory of captured sessions.
    ///   GIT_MESH_SUGGEST_TRIGRAM=0     Disable trigram cohesion (mirrors JS --no-trigram).
    ///   GIT_MESH_SUGGEST_HISTORY=0     Disable historical-cochange channel (mirrors JS --no-history).
    ///   GIT_MESH_SUGGEST_FIXTURE=1     Disable history channel for fixture/parity runs (prevents repo contamination).
    ///   GIT_MESH_SUGGEST_REPO_ROOT     Override repo root for cohesion file reads (default: cwd).
    Suggest,
}

/// Top-level entry: dispatches to `snapshot`, `read`, `milestone`, `stop`, or
/// `suggest`. No bare-render arm — a subcommand is always required.
///
/// `Suggest` does not require a `<SESSION_ID>` — it is corpus-wide and reads
/// every session from `GIT_MESH_ADVICE_DIR`. All other subcommands require a
/// session id and fail closed when one is not provided.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    // Suggest is corpus-wide — no session id required.
    if matches!(args.command, Some(AdviceCommand::Suggest)) {
        return run_advice_suggest();
    }
    // All other subcommands are session-scoped.
    let session_id = args.session_id.ok_or_else(|| {
        anyhow::anyhow!("git mesh advice: a <SESSION_ID> is required (e.g. `git mesh advice <id>`)")
    })?;
    validate_session_id(&session_id)?;
    match args.command {
        Some(AdviceCommand::Snapshot) => run_advice_snapshot(repo, session_id),
        Some(AdviceCommand::Read { anchor }) => run_advice_read(repo, session_id, anchor),
        Some(AdviceCommand::Milestone) => run_advice_milestone(repo, session_id),
        Some(AdviceCommand::Stop) => run_advice_stop(repo, session_id),
        Some(AdviceCommand::Suggest) => unreachable!("handled above"),
        None => bail!(
            "git mesh advice: a subcommand is required; run `git mesh advice --help` for usage"
        ),
    }
}

/// Delta render pipeline — internal helper preserved for Phase 3 to reuse.
/// Not exposed on the public CLI surface; `milestone` and `stop` will call
/// this (or a derivative) when their behaviour is implemented in Phase 3.
#[allow(dead_code)]
fn run_advice_render(
    repo: &gix::Repository,
    session_id: &str,
    documentation: bool,
    snapshot_if_missing: bool,
) -> Result<i32> {
    use crate::advice::candidates::{
        CandidateInput, MeshAnchor, MeshAnchorStatus, StagedAddr, StagingState,
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

    crate::advice_debug!(
        "cli-entry",
        "sid" => session_id,
        "repo" => wd.display(),
        "documentation" => documentation,
        "snapshot_if_missing" => snapshot_if_missing
    );

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
    let mesh_anchors: Vec<MeshAnchor> =
        match crate::resolver::all_meshes(repo, default_engine_options()) {
            Ok(meshes) => meshes
                .into_iter()
                .flat_map(|m| {
                    let name = m.name.clone();
                    let why = m.message.clone();
                    m.anchors.into_iter().map(move |r| {
                        let whole =
                            matches!(r.anchored.extent, crate::types::AnchorExtent::WholeFile);
                        MeshAnchor {
                            name: name.clone(),
                            why: why.clone(),
                            path: std::path::PathBuf::from(
                                r.anchored.path.to_string_lossy().into_owned(),
                            ),
                            start: match r.anchored.extent {
                                crate::types::AnchorExtent::LineRange { start, .. } => start,
                                crate::types::AnchorExtent::WholeFile => 0,
                            },
                            end: match r.anchored.extent {
                                crate::types::AnchorExtent::LineRange { end, .. } => end,
                                crate::types::AnchorExtent::WholeFile => u32::MAX,
                            },
                            whole,
                            status: match r.status {
                                crate::types::AnchorStatus::Fresh => MeshAnchorStatus::Stable,
                                crate::types::AnchorStatus::Moved => MeshAnchorStatus::Moved,
                                crate::types::AnchorStatus::Changed => MeshAnchorStatus::Changed,
                                _ => MeshAnchorStatus::Terminal,
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
                let whole = matches!(add.extent, crate::types::AnchorExtent::WholeFile);
                let (s, e) = match add.extent {
                    crate::types::AnchorExtent::LineRange { start, end } => (start, end),
                    crate::types::AnchorExtent::WholeFile => (0, u32::MAX),
                };
                staging_adds.push(StagedAddr {
                    path: std::path::PathBuf::from(add.path),
                    start: s,
                    end: e,
                    whole,
                });
            }
            for rem in staging.removes {
                let whole = matches!(rem.extent, crate::types::AnchorExtent::WholeFile);
                let (s, e) = match rem.extent {
                    crate::types::AnchorExtent::LineRange { start, end } => (start, end),
                    crate::types::AnchorExtent::WholeFile => (0, u32::MAX),
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
    let meshes_seen = store.meshes_seen_set()?;
    let touch_intervals = store.all_touch_intervals()?;

    // Step 13: produce candidates.
    let input = CandidateInput {
        session_delta: &session_delta,
        incr_delta: &incr_delta,
        new_reads: &new_reads,
        touch_intervals: &touch_intervals,
        mesh_anchors: &mesh_anchors,
        internal_path_prefixes: &internal_path_prefixes,
        staging: StagingState {
            adds: &staging_adds,
            removes: &staging_removes,
        },
    };
    let mut candidates: Vec<crate::advice::candidates::Candidate> = Vec::new();
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_read_intersects_mesh(
            &input,
        ));
        crate::advice_debug!("detector", "name" => "detect_read_intersects_mesh", "candidates" => candidates.len() - before);
    }
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_delta_intersects_mesh(
            &input,
        ));
        crate::advice_debug!("detector", "name" => "detect_delta_intersects_mesh", "candidates" => candidates.len() - before);
    }
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_partner_drift(&input));
        crate::advice_debug!("detector", "name" => "detect_partner_drift", "candidates" => candidates.len() - before);
    }
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_rename_consequence(&input));
        crate::advice_debug!("detector", "name" => "detect_rename_consequence", "candidates" => candidates.len() - before);
    }
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_range_shrink(&input));
        crate::advice_debug!("detector", "name" => "detect_range_shrink", "candidates" => candidates.len() - before);
    }
    {
        let before = candidates.len();
        candidates.extend(crate::advice::candidates::detect_staging_cross_cut(&input));
        crate::advice_debug!("detector", "name" => "detect_staging_cross_cut", "candidates" => candidates.len() - before);
    }
    // Card main-13 slice 2: the pairwise `detect_session_co_touch` channel was
    // replaced by the n-ary, line-bounded suggester; its output is folded into
    // `kept_suggestions` below after fingerprint-dedup.

    // Step 14: filter out fingerprints in advice_seen_set, and drop any
    // candidate whose mesh has already surfaced in a prior render of this
    // session (every mesh is announced at most once per advice session).
    // Within a single render, all partners for a freshly-surfaced mesh are
    // emitted together; we record the mesh name afterward so subsequent
    // renders suppress it.
    let mut emitted_fps: Vec<String> = Vec::new();
    let kept: Vec<crate::advice::candidates::Candidate> = candidates
        .into_iter()
        .filter_map(|c| {
            let fp = crate::advice::fingerprint::fingerprint(&c);
            if advice_seen.contains(&fp) {
                crate::advice_debug!(
                    "dropped",
                    "reason" => "advice-seen",
                    "fp" => fp,
                    "mesh" => c.mesh
                );
                return None;
            }
            if !c.mesh.is_empty() && meshes_seen.contains(&c.mesh) {
                crate::advice_debug!(
                    "dropped",
                    "reason" => "meshes-seen",
                    "fp" => fp,
                    "mesh" => c.mesh
                );
                return None;
            }
            crate::advice_debug!(
                "kept",
                "fp" => fp,
                "mesh" => c.mesh,
                "reason_kind" => c.reason_kind,
                "partner" => c.partner_path
            );
            emitted_fps.push(fp);
            Some(c)
        })
        .collect();
    let new_meshes_seen: Vec<String> = {
        let mut seen_in_render: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        kept.iter()
            .filter_map(|c| {
                if c.mesh.is_empty() {
                    None
                } else if seen_in_render.insert(c.mesh.clone()) {
                    Some(c.mesh.clone())
                } else {
                    None
                }
            })
            .collect()
    };

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
                } else {
                    crate::advice_debug!("dropped", "reason" => "docs-seen", "topic" => key);
                }
            }
        }
        emitted_topics.into_iter().collect()
    } else {
        Vec::new()
    };
    // Convert candidates to Suggestions at the Detector seam before rendering.
    let mut kept_suggestions: Vec<crate::advice::suggestion::Suggestion> = kept
        .iter()
        .map(crate::advice::candidates::candidate_to_suggestion)
        .collect();

    // Card main-13 slice 2: append n-ary, line-bounded mesh recommendations
    // produced by the suggester pipeline (replaces the pairwise
    // `detect_session_co_touch` channel). Fail-soft: if the corpus loader
    // returns nothing, the suggester is skipped and the per-session render
    // continues unchanged.
    {
        use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};
        let advice_dir = match std::env::var("GIT_MESH_ADVICE_DIR") {
            Ok(s) if !s.is_empty() => std::path::PathBuf::from(s),
            _ => store
                .dir()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        };
        let sessions = if advice_dir.as_os_str().is_empty() {
            Vec::new()
        } else {
            load_all_sessions(&advice_dir).unwrap_or_default()
        };
        if !sessions.is_empty() {
            crate::advice_debug!("suggester-entry", "sessions" => sessions.len());
            let cfg = SuggestConfig::from_env();
            let suggester_out = run_suggest_pipeline(&sessions, Some(repo), wd, &cfg);
            let mut dropped_seen = 0usize;
            let before = kept_suggestions.len();
            for sug in suggester_out {
                let fp = crate::advice::fingerprint::fingerprint_suggestion(&sug);
                if advice_seen.contains(&fp) {
                    dropped_seen += 1;
                    continue;
                }
                emitted_fps.push(fp);
                kept_suggestions.push(sug);
            }
            crate::advice_debug!(
                "suggester-exit",
                "emitted" => kept_suggestions.len() - before,
                "dropped_seen" => dropped_seen
            );
        }
    }

    let rendered = crate::advice::render::render(&kept_suggestions, &new_doc_topics, documentation);

    // Build touch intervals (finding 3): one per affected path/anchor from
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
    if !new_meshes_seen.is_empty() {
        store.append_meshes_seen(&new_meshes_seen)?;
    }
    if !new_doc_topics.is_empty() {
        store.append_docs_seen(&new_doc_topics)?;
    }
    for interval in &touch_intervals_to_append {
        store.append_touch(interval)?;
    }

    crate::advice_debug!(
        "cli-exit",
        "suggestions" => kept_suggestions.len(),
        "doc_topics" => new_doc_topics.len()
    );

    Ok(0)
}

/// Translate a session's `incr_delta` + `new_reads` into one
/// `TouchInterval` per affected path/anchor, all sharing `ts` so a co-touch
/// detector can group them by timestamp. (Finding 3.)
#[allow(dead_code)]
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
                    crate::advice_debug!("dropped", "reason" => "internal-path", "path" => path);
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
                } else {
                    crate::advice_debug!("dropped", "reason" => "internal-path", "path" => from);
                }
                if !advice_path_is_internal(to, internal_path_prefixes) {
                    out.push(TouchInterval {
                        path: to.clone(),
                        start_line: 0,
                        end_line: 0,
                        ts: ts.to_string(),
                    });
                } else {
                    crate::advice_debug!("dropped", "reason" => "internal-path", "path" => to);
                }
            }
        }
    }
    for r in new_reads {
        if advice_path_is_internal(&r.path, internal_path_prefixes) {
            crate::advice_debug!("dropped", "reason" => "internal-path", "path" => r.path);
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
#[allow(dead_code)]
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
    repo.git_dir()
        .join("objects")
        .join(dir)
        .join(file)
        .is_file()
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
        needs_all_layers: true,
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
        bail!("{}", no_sessions_error_message(&advice_dir));
    }

    // Attempt to open the git repo for the history channel.
    // History is disabled when:
    //   - GIT_MESH_SUGGEST_HISTORY=0 (explicit user knob), OR
    //   - GIT_MESH_SUGGEST_FIXTURE=1 (parity/fixture run — prevents repo contamination)
    // Using GIT_MESH_ADVICE_DIR to gate history has been removed: a custom advice
    // directory is a valid user configuration and must not silently degrade quality.
    let fixture_mode = std::env::var("GIT_MESH_SUGGEST_FIXTURE").as_deref() == Ok("1");
    if fixture_mode {
        eprintln!(
            "fixture mode: GIT_MESH_SUGGEST_FIXTURE is set; historical-cochange channel disabled"
        );
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

    let suggestions = run_suggest_pipeline(&sessions, repo_opt.as_ref(), &repo_root, &cfg);

    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for s in &suggestions {
        let line =
            serde_json::to_string(s).map_err(|e| anyhow::anyhow!("serialize suggestion: {e}"))?;
        writeln!(handle, "{line}").map_err(|e| anyhow::anyhow!("write suggestion: {e}"))?;
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
        let v: T = serde_json::from_str(&line)
            .map_err(|e| anyhow::anyhow!("parse `{}` line {}: {e}", path.display(), idx + 1))?;
        out.push(v);
    }
    Ok(out)
}

/// Load all sessions from a directory by scanning subdirectories.
///
/// Layout classification (per finding 1 and finding 2):
///
/// **Repo-key isolation (finding 1):**
/// - When `gix::discover(".")` succeeds AND we are not in fixture mode: only sessions under
///   `<base>/<preferred_key>/` are eligible. If none exist for that key, an empty `Vec` is
///   returned (the outer fail-closed `bail!` then fires with a message naming the key).
/// - When `gix::discover(".")` fails (not in a repo) OR `GIT_MESH_SUGGEST_FIXTURE=1`: all
///   flat sessions and all two-level sessions across all keys are eligible (cross-corpus).
///
/// **Flat-vs-nested classification (finding 2):**
/// A directory at the first level is classified as:
/// - **session dir** (flat): has `reads.jsonl`/`touches.jsonl` AND has NO subdirectories
///   that themselves contain `reads.jsonl`/`touches.jsonl`.
/// - **repo_key dir** (nested): has at least one subdirectory that contains
///   `reads.jsonl`/`touches.jsonl`.
/// - **ambiguous**: has both session files AND nested session subdirs. A stderr warning is
///   emitted and the nested interpretation is preferred (real sessions not silently dropped).
/// - **empty/unrelated**: skipped silently.
fn load_all_sessions(dir: &std::path::Path) -> Result<Vec<crate::advice::suggest::SessionRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let fixture_mode = std::env::var("GIT_MESH_SUGGEST_FIXTURE").as_deref() == Ok("1");

    // Compute the current repo key. None means "not in a repo" → cross-corpus mode.
    let preferred_key: Option<String> = gix::discover(".").ok().and_then(|repo| {
        let root = repo.workdir().map(|p| p.to_path_buf())?;
        let git_dir = repo.git_dir().to_path_buf();
        Some(crate::advice::session::store::repo_key(&root, &git_dir))
    });

    // Cross-corpus mode: fixture mode OR not inside any repo.
    let cross_corpus = fixture_mode || preferred_key.is_none();

    /// Returns all subdirectories of `session_root` that contain
    /// `reads.jsonl` or `touches.jsonl` (i.e. valid session dirs).
    fn nested_session_dirs(session_root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
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
            sessions.push(SessionRecord {
                sid,
                reads,
                touches,
            });
        }
        Ok(sessions)
    }

    // Classify each first-level subdirectory.
    let mut flat_dirs: Vec<std::path::PathBuf> = Vec::new();
    // Maps repo_key string → session dirs collected from that key dir.
    let mut keyed_sessions: std::collections::HashMap<String, Vec<std::path::PathBuf>> =
        std::collections::HashMap::new();

    let first_level: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read_dir `{}`: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|e| e.path())
        .collect();

    for entry in first_level {
        let has_own_session_files =
            entry.join("reads.jsonl").exists() || entry.join("touches.jsonl").exists();
        let nested = nested_session_dirs(&entry)?;
        let has_nested_sessions = !nested.is_empty();

        match (has_own_session_files, has_nested_sessions) {
            (true, false) => {
                // Pure flat/fixture session dir.
                flat_dirs.push(entry);
            }
            (false, true) => {
                // Pure repo_key dir — descend.
                let key = entry
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                keyed_sessions.entry(key).or_default().extend(nested);
            }
            (true, true) => {
                // Ambiguous: stray session files at the key level AND real nested sessions.
                // Prefer nested (so real sessions are not silently dropped) and warn.
                eprintln!(
                    "git mesh advice suggest: warning: `{}` contains both session files \
                     (reads.jsonl/touches.jsonl) and session subdirectories; \
                     treating as a repo-key directory (nested sessions preferred)",
                    entry.display()
                );
                let key = entry
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                keyed_sessions.entry(key).or_default().extend(nested);
            }
            (false, false) => {
                // Empty or unrelated directory — skip silently.
            }
        }
    }

    // Assemble the chosen session dirs according to repo-key isolation rules.
    let mut chosen_dirs: Vec<std::path::PathBuf> = Vec::new();

    if cross_corpus {
        // Fixture mode or not in a repo: accept all flat + all keyed sessions.
        chosen_dirs.extend(flat_dirs);
        for (_, dirs) in keyed_sessions {
            chosen_dirs.extend(dirs);
        }
    } else {
        // Strict repo-key isolation: ONLY sessions under <base>/<preferred_key>/<sid>/ are
        // eligible. Flat dirs at <base>/<sid>/ are foreign to the current repo (hand-authored
        // fixture, leftover from a stray invocation, copied corpus, etc.) and must NOT be
        // loaded silently. Emit a warning if any flat dirs were found so the operator can
        // set GIT_MESH_SUGGEST_FIXTURE=1 to include them explicitly.
        if !flat_dirs.is_empty() {
            eprintln!(
                "warning: skipping {} top-level flat session dir(s) under `{}` \
                 (only sessions under `{}/{}` are eligible in strict mode; \
                 set GIT_MESH_SUGGEST_FIXTURE=1 to include them)",
                flat_dirs.len(),
                dir.display(),
                dir.display(),
                preferred_key.as_deref().unwrap_or("<repo-key>"),
            );
        }
        let pk = preferred_key.as_deref().unwrap_or("");
        if let Some(dirs) = keyed_sessions.remove(pk) {
            chosen_dirs.extend(dirs);
        }
        // If preferred_key had no sessions: chosen_dirs is empty.
        // The outer fail-closed bail! will fire with a message naming the key.
    }

    chosen_dirs.sort();
    sessions_from_dirs(chosen_dirs)
}

/// Format a "no sessions found" error message that names the preferred repo key when
/// we are inside a repo, so the user can locate where sessions should be written.
pub(crate) fn no_sessions_error_message(advice_dir: &std::path::Path) -> String {
    let preferred_key: Option<String> = gix::discover(".").ok().and_then(|repo| {
        let root = repo.workdir().map(|p| p.to_path_buf())?;
        let git_dir = repo.git_dir().to_path_buf();
        Some(crate::advice::session::store::repo_key(&root, &git_dir))
    });
    let fixture_mode = std::env::var("GIT_MESH_SUGGEST_FIXTURE").as_deref() == Ok("1");
    match (preferred_key, fixture_mode) {
        (Some(key), false) => format!(
            "no sessions found under `{}/{key}`; a session directory must contain \
             reads.jsonl or touches.jsonl",
            advice_dir.display()
        ),
        (None, false) => format!(
            "no sessions found under `{}` (running outside a repo: cross-corpus mode); \
             a session directory must contain reads.jsonl or touches.jsonl",
            advice_dir.display()
        ),
        _ => format!(
            "no sessions found under `{}`; a session directory must contain \
             reads.jsonl or touches.jsonl",
            advice_dir.display()
        ),
    }
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

/// Record a single read event (one anchor or whole-file path) in the
/// file-backed session store. Emits `BasicOutput` for each first-time matching
/// mesh, appends the mesh name to `meshes-seen.jsonl` and `mesh-candidates.jsonl`.
fn run_advice_read(repo: &gix::Repository, session_id: String, anchor: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::ReadRecord;
    use crate::advice::session::store::LockTimeout;
    use crate::advice::structured::{
        BasicOutput, Status, action_from_spec, format_anchor_resolved, read_overlaps,
    };

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;

    if anchor.is_empty() {
        bail!("git mesh advice <id> read: anchor must not be empty");
    }

    validate_read_spec(repo, &anchor)?;

    // Require baseline.state — fail closed.
    if !store.dir().join("baseline.state").exists() {
        bail!(
            "no baseline for session `{session_id}`; run snapshot first \
             (`git mesh advice {session_id} snapshot`)"
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let (path_str, line_anchor) = match anchor.split_once("#L") {
        Some((p, frag)) => {
            let (s, e) = frag.split_once("-L").unwrap();
            (
                p.to_string(),
                Some((s.parse::<u32>().unwrap(), e.parse::<u32>().unwrap())),
            )
        }
        None => (anchor.clone(), None),
    };
    let rec = ReadRecord {
        path: path_str,
        start_line: line_anchor.map(|(s, _)| s),
        end_line: line_anchor.map(|(_, e)| e),
        ts: now,
    };
    store.append_read(
        &rec,
        LockTimeout::Bounded(std::time::Duration::from_secs(30)),
    )?;

    // Build the Action for overlap checking.
    let action = action_from_spec(&anchor).ok_or_else(|| {
        anyhow::anyhow!("internal: action_from_spec returned None for `{anchor}`")
    })?;

    // Load mesh state. Treat any error as empty (greenfield).
    let meshes = crate::resolver::all_meshes(repo, default_engine_options()).unwrap_or_default();

    // Load per-session dedup sets.
    let meshes_seen = store.meshes_seen_set()?;

    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();

    for mesh in &meshes {
        // Check if any anchor in this mesh overlaps the action.
        let matching_anchor = mesh.anchors.iter().find(|a| read_overlaps(&action, a));
        let Some(active_anchor_resolved) = matching_anchor else {
            continue;
        };

        // Rule 1: skip if already in meshes-seen.
        if meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name) {
            continue;
        }

        // Build BasicOutput.
        let active_anchor_str = format_anchor_resolved(active_anchor_resolved);
        let status_if_not_fresh = if matches!(
            active_anchor_resolved.status,
            crate::types::AnchorStatus::Fresh
        ) {
            None
        } else {
            Some(Status::from_anchor_status(&active_anchor_resolved.status))
        };
        let non_active_anchors: Vec<String> = mesh
            .anchors
            .iter()
            .filter(|a| a.anchor_id != active_anchor_resolved.anchor_id)
            .map(format_anchor_resolved)
            .collect();

        let block = BasicOutput {
            active_anchor: active_anchor_str,
            mesh_name: mesh.name.clone(),
            why: mesh.message.clone(),
            status_if_not_fresh,
            non_active_anchors,
        };
        blocks.push(block.to_string());

        // Rule 3 & 4: record mesh as seen and as candidate.
        new_meshes_seen.push(mesh.name.clone());
        new_mesh_candidates.push(mesh.name.clone());
    }

    let output = if blocks.is_empty() {
        String::new()
    } else {
        let mut out = String::from("\n\n");
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                out.push_str("\n---\n\n");
            }
            out.push_str(b);
        }
        out
    };

    // Write output before persisting state (fail-open: if stdout fails,
    // do not advance seen sets so the mesh resurfaces).
    use std::io::Write;
    if !output.is_empty() {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let result = handle
            .write_all(output.as_bytes())
            .and_then(|()| handle.flush());
        match result {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                return Err(anyhow::Error::from(e).context("write advice to stdout"));
            }
        }
    }

    if !new_meshes_seen.is_empty() {
        store.append_meshes_seen(&new_meshes_seen)?;
    }
    if !new_mesh_candidates.is_empty() {
        store.append_mesh_candidates(&new_mesh_candidates)?;
    }

    Ok(0)
}

/// Flush the current session delta: emit advice for every implicit dependency
/// crossed since the last milestone or snapshot.
///
/// Implements the structured-English EDIT rule (§"EDIT — …") and the CREATE
/// rule (§"CREATE — new file matching a RELATED_ANCHORS entry …") for the
/// `milestone` verb. Session flags gate instruction blocks to at most one
/// emission per session.
fn run_advice_milestone(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::structured::{
        Action, BasicOutput, Status, creation_instructions, edit_overlaps, format_anchor_resolved,
        mesh_is_stale, reconciliation_instructions,
    };
    use crate::advice::workspace_tree::{self, DiffEntry};

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;

    // Step 1a: require baseline.state.
    if !store.dir().join("baseline.state").exists() {
        bail!(
            "no baseline for session `{session_id}`; run snapshot first \
             (`git mesh advice {session_id} snapshot`)"
        );
    }
    let baseline = store.read_baseline()?;

    // Step 1b: load SessionFlags.
    let mut flags = store.read_flags()?;

    // Step 2: capture current workspace tree.
    let cur_uuid = uuid::Uuid::new_v4();
    let current_objects = store.dir().join(format!("current.objects-{cur_uuid}"));
    std::fs::create_dir_all(&current_objects)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", current_objects.display()))?;

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

    let current_objects_guard = DirGuard::new(current_objects.clone());
    let current = workspace_tree::capture(repo, &current_objects)?;

    // Step 3: diff_trees(baseline → current) for FILES_MODIFIED_DURING_SESSION.
    let baseline_objects = store.baseline_objects_dir();
    let session_delta = workspace_tree::diff_trees(
        repo,
        &baseline.tree_sha,
        &current.tree_sha,
        &baseline_objects,
        &current_objects,
    )?;

    // Step 4: last-flush state and objects for the incremental delta.
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

    // Step 5: diff_trees(last_flush → current) for touch intervals.
    let last_flush_objects = last_flush_objects_path;
    let incr_delta = workspace_tree::diff_trees(
        repo,
        &last_flush_state.tree_sha,
        &current.tree_sha,
        &last_flush_objects_for_diff,
        &current_objects,
    )?;

    let internal_path_prefixes = active_advice_store_prefixes(wd, store.dir());

    // Step 6: load mesh state.
    let meshes = crate::resolver::all_meshes(repo, default_engine_options()).unwrap_or_default();

    // Step 7: load per-session dedup sets.
    let meshes_seen = store.meshes_seen_set()?;

    // ── EDIT rule ─────────────────────────────────────────────────────────────
    // Materialise FILES_MODIFIED_DURING_SESSION from session_delta.
    // DiffEntry::Modified → Action::WholeFile (no hunk bounds from diff --raw).
    // DiffEntry::Removed  → skip (no meaningful anchor overlap for deletions).
    // DiffEntry::Added    → handled separately in CREATE rule below.

    let mut output = String::new();
    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();
    // F4: per-call dedup — tracks meshes already emitted in this verb invocation.
    let mut emitted_meshes_this_call: Vec<String> = Vec::new();

    for entry in &session_delta {
        let path = match entry {
            DiffEntry::Modified { path, .. } => path.clone(),
            DiffEntry::ModeChange { path, .. } => path.clone(),
            DiffEntry::Renamed { to, .. } => to.clone(),
            // Added paths are handled by the CREATE rule below.
            DiffEntry::Added { .. } => continue,
            // Deleted paths: no anchor overlap meaningful.
            DiffEntry::Deleted { .. } => continue,
        };
        if advice_path_is_internal(&path, &internal_path_prefixes) {
            continue;
        }
        let action = Action::WholeFile { path };

        for mesh in &meshes {
            // F4: per-call dedup — skip if already emitted in this invocation.
            if emitted_meshes_this_call.contains(&mesh.name) {
                continue;
            }
            // Find any anchor that overlaps the action.
            let matching_anchor = mesh.anchors.iter().find(|a| edit_overlaps(&action, a));
            let Some(active_anchor_resolved) = matching_anchor else {
                continue;
            };

            // EDIT rule 1: if NOT mesh_is_stale AND mesh is in meshes_seen → skip.
            let stale = mesh_is_stale(mesh);
            let already_seen =
                meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name);
            if !stale && already_seen {
                continue;
            }

            // Emit BasicOutput.
            let active_anchor_str = format_anchor_resolved(active_anchor_resolved);
            let status_if_not_fresh = if matches!(
                active_anchor_resolved.status,
                crate::types::AnchorStatus::Fresh
            ) {
                None
            } else {
                Some(Status::from_anchor_status(&active_anchor_resolved.status))
            };
            let non_active_anchors: Vec<String> = mesh
                .anchors
                .iter()
                .filter(|a| a.anchor_id != active_anchor_resolved.anchor_id)
                .map(format_anchor_resolved)
                .collect();
            let block = BasicOutput {
                active_anchor: active_anchor_str,
                mesh_name: mesh.name.clone(),
                why: mesh.message.clone(),
                status_if_not_fresh,
                non_active_anchors,
            };
            output.push_str(&block.to_string());
            emitted_meshes_this_call.push(mesh.name.clone());

            // Record mesh as seen and as candidate (whether or not stale).
            if !already_seen {
                new_meshes_seen.push(mesh.name.clone());
            }
            if !new_mesh_candidates.contains(&mesh.name) {
                new_mesh_candidates.push(mesh.name.clone());
            }

            // EDIT rule 3: reconciliation instructions gate.
            if !flags.has_printed_reconciliation_instructions {
                output.push_str(&reconciliation_instructions(mesh));
                flags.has_printed_reconciliation_instructions = true;
            }
        }
    }

    // ── CREATE rule ───────────────────────────────────────────────────────────
    // For each Added path in session_delta, run the suggester pipeline and
    // emit a per-anchor hint for high-confidence, not-yet-suggested entries.

    let added_paths: Vec<String> = session_delta
        .iter()
        .filter_map(|e| match e {
            DiffEntry::Added { path, .. } => {
                if advice_path_is_internal(path, &internal_path_prefixes) {
                    None
                } else {
                    Some(path.clone())
                }
            }
            _ => None,
        })
        .collect();

    let mut emitted_fps: Vec<String> = Vec::new();
    let mut any_creation_emission = false;

    if !added_paths.is_empty() {
        use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};

        let advice_dir = match std::env::var("GIT_MESH_ADVICE_DIR") {
            Ok(s) if !s.is_empty() => std::path::PathBuf::from(s),
            _ => store
                .dir()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        };
        let sessions = if advice_dir.as_os_str().is_empty() {
            Vec::new()
        } else {
            load_all_sessions(&advice_dir).unwrap_or_default()
        };

        if !sessions.is_empty() {
            let cfg = SuggestConfig::from_env();
            let suggestions = run_suggest_pipeline(&sessions, Some(repo), wd, &cfg);
            let advice_seen = store.advice_seen_set()?;

            for sug in &suggestions {
                use crate::advice::suggestion::ConfidenceBand;
                // CREATE rule: confidence == high ↔ band ∈ {High, HighPlus}.
                if !matches!(sug.band, ConfidenceBand::High | ConfidenceBand::HighPlus) {
                    continue;
                }
                let fp = crate::advice::fingerprint::fingerprint_suggestion(sug);
                // suggested == false ↔ fingerprint absent from advice-seen.jsonl.
                if advice_seen.contains(&fp) || emitted_fps.contains(&fp) {
                    continue;
                }

                // Check that at least one participant overlaps an added path.
                let has_added_participant = sug.participants.iter().any(|p| {
                    added_paths.iter().any(|ap| {
                        let p_path = p.path.to_string_lossy();
                        p_path.as_ref() == ap.as_str()
                    })
                });
                if !has_added_participant {
                    continue;
                }

                // Emit per-anchor hint line per the spec:
                // "If [ACTIVE_ANCHOR] has implicit semantic dependencies…"
                for p in &sug.participants {
                    let active_anchor = if p.whole {
                        p.path.to_string_lossy().into_owned()
                    } else {
                        format!("{}#L{}-L{}", p.path.to_string_lossy(), p.start, p.end)
                    };
                    output.push_str(&format!(
                        "If {active_anchor} has implicit semantic dependencies, document with `git mesh`:\n"
                    ));
                    output.push_str(
                        "  git mesh add <name> [anchor 1] [anchor 2] && git mesh why -m <why>\n",
                    );
                }

                emitted_fps.push(fp);
                any_creation_emission = true;
            }
        }

        if any_creation_emission && !flags.has_printed_creation_instructions {
            // creation_instructions takes &[&AnchorResolved]; we have no resolved
            // anchors for the added paths at this point (they're new files with
            // no existing mesh anchors). Emit the fixed instruction block directly
            // using the text from creation_instructions with an empty slice.
            let creation_text = creation_instructions(&[]);
            output.push_str(&creation_text);
            flags.has_printed_creation_instructions = true;
        }
    }

    // ── Step 5 / advance last-flush ───────────────────────────────────────────
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

    let new_cursor = store.reads_byte_len()?;
    let new_last_flush = crate::advice::session::state::BaselineState {
        schema_version: crate::advice::session::SCHEMA_VERSION,
        tree_sha: current.tree_sha.clone(),
        index_sha: baseline.index_sha.clone(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        read_cursor: new_cursor,
    };
    store.write_last_flush(&new_last_flush)?;

    // ── Write output ──────────────────────────────────────────────────────────
    use std::io::Write;
    let stdout_result = if output.is_empty() {
        Ok(())
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(output.as_bytes())
            .and_then(|()| handle.flush())
    };
    match stdout_result {
        Ok(()) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            return Err(anyhow::Error::from(e).context("write advice to stdout"));
        }
    }

    // Persist seen sets and flags only after successful output.
    if !new_meshes_seen.is_empty() {
        store.append_meshes_seen(&new_meshes_seen)?;
    }
    if !new_mesh_candidates.is_empty() {
        store.append_mesh_candidates(&new_mesh_candidates)?;
    }
    if !emitted_fps.is_empty() {
        store.append_advice_seen(&emitted_fps)?;
    }

    // Build touch intervals from incr_delta.
    let touch_ts = chrono::Utc::now().to_rfc3339();
    let touch_intervals = build_touch_intervals(
        incr_delta.as_slice(),
        &[],
        &touch_ts,
        &internal_path_prefixes,
    );
    for interval in &touch_intervals {
        store.append_touch(interval)?;
    }

    // Step 6: persist SessionFlags.
    store.write_flags(&flags)?;

    Ok(0)
}

/// Flush the session and emit a final reconciliation sweep for any
/// touched-and-stale meshes not yet announced, followed by a creation sweep
/// for high-confidence related-anchor suggestions.
///
/// Internally performs the same EDIT+CREATE flush as `milestone` (with the
/// same dedup rules) before the stop-specific Block A and Block B sweeps.
/// All four passes share a single store session so their seen/candidate sets
/// compose correctly without creating duplicate JSONL entries.
fn run_advice_stop(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::structured::{
        Action, BasicOutput, Status, creation_instructions, edit_overlaps, format_anchor_resolved,
        mesh_is_stale, reconciliation_instructions,
    };
    use crate::advice::workspace_tree::{self, DiffEntry};

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
    let baseline = store.read_baseline()?;

    // Step 3: capture current workspace tree for session_delta.
    let cur_uuid = uuid::Uuid::new_v4();
    let current_objects = store.dir().join(format!("current.objects-{cur_uuid}"));
    std::fs::create_dir_all(&current_objects)
        .map_err(|e| anyhow::anyhow!("mkdir `{}`: {e}", current_objects.display()))?;

    struct StopDirGuard(std::path::PathBuf);
    impl Drop for StopDirGuard {
        fn drop(&mut self) {
            if self.0.exists() {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    let _current_objects_guard = StopDirGuard(current_objects.clone());
    let current = workspace_tree::capture(repo, &current_objects)?;

    // Step 4: diff_trees(baseline → current) for FILES_MODIFIED_DURING_SESSION.
    let baseline_objects = store.baseline_objects_dir();
    let session_delta = workspace_tree::diff_trees(
        repo,
        &baseline.tree_sha,
        &current.tree_sha,
        &baseline_objects,
        &current_objects,
    )?;

    // Step 4b: last-flush state and incr_delta for the milestone-equivalent pass.
    let last_flush_state_path = store.dir().join("last-flush.state");
    let last_flush_objects_path = store.last_flush_objects_dir();
    let (last_flush_state, last_flush_objects_for_diff) = if last_flush_state_path.exists() {
        let st = store.read_last_flush()?;
        let consistent = last_flush_objects_path.exists()
            && tree_resolves_in(repo, &st.tree_sha, &last_flush_objects_path);
        if consistent {
            (st, last_flush_objects_path.clone())
        } else {
            (baseline.clone(), baseline_objects.clone())
        }
    } else {
        (baseline.clone(), baseline_objects.clone())
    };
    let last_flush_objects = last_flush_objects_path;
    let incr_delta = workspace_tree::diff_trees(
        repo,
        &last_flush_state.tree_sha,
        &current.tree_sha,
        &last_flush_objects_for_diff,
        &current_objects,
    )?;

    let internal_path_prefixes = active_advice_store_prefixes(wd, store.dir());

    // Step 5: load mesh state.
    let meshes = crate::resolver::all_meshes(repo, default_engine_options()).unwrap_or_default();

    // Step 6: load per-session dedup sets.
    let meshes_seen = store.meshes_seen_set()?;
    let mesh_candidates = store.mesh_candidates_set()?;

    // Step 7: load SessionFlags.
    let mut flags = store.read_flags()?;

    let mut output = String::new();
    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();

    // ── Milestone-equivalent flush pass ──────────────────────────────────────
    // Same EDIT rule as `milestone`, but running inside stop's single store
    // session so the seen/candidate sets compose without creating JSONL dupes.
    // Unlike the standalone `milestone`, we also skip meshes already in
    // mesh_candidates (the session is ending; re-announcing a known candidate
    // adds no value and creates duplicate JSONL entries).
    //
    // IMPORTANT: iterate session_delta (baseline → current), NOT incr_delta
    // (last_flush → current). When the user runs `milestone` mid-session and a
    // mesh becomes stale afterward, stop's EDIT pass must still reach the file
    // even if it's outside incr_delta. session_delta covers the full session.
    let mut emitted_meshes_this_call: Vec<String> = Vec::new();
    for entry in &session_delta {
        let path = match entry {
            DiffEntry::Modified { path, .. } => path.clone(),
            DiffEntry::ModeChange { path, .. } => path.clone(),
            DiffEntry::Renamed { to, .. } => to.clone(),
            DiffEntry::Added { .. } => continue,
            DiffEntry::Deleted { .. } => continue,
        };
        if advice_path_is_internal(&path, &internal_path_prefixes) {
            continue;
        }
        let action = Action::WholeFile { path };

        for mesh in &meshes {
            // F4: per-call dedup — skip if already emitted in this invocation.
            if emitted_meshes_this_call.contains(&mesh.name) {
                continue;
            }
            let already_seen =
                meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name);
            let already_candidate =
                mesh_candidates.contains(&mesh.name) || new_mesh_candidates.contains(&mesh.name);
            let stale = mesh_is_stale(mesh);
            // Skip: already announced, already a candidate, and NOT stale.
            // A stale mesh must still be re-emitted even if previously seen/candidate.
            if already_seen && already_candidate && !stale {
                continue;
            }
            // Skip: not stale and already announced (same as milestone EDIT rule 1).
            if !stale && already_seen {
                continue;
            }
            if !mesh.anchors.iter().any(|a| edit_overlaps(&action, a)) {
                continue;
            }

            let active_anchor_resolved = mesh.anchors.iter().find(|a| edit_overlaps(&action, a));
            let Some(active) = active_anchor_resolved else {
                continue;
            };

            let active_anchor_str = format_anchor_resolved(active);
            let status_if_not_fresh = if matches!(active.status, crate::types::AnchorStatus::Fresh)
            {
                None
            } else {
                Some(Status::from_anchor_status(&active.status))
            };
            let non_active_anchors: Vec<String> = mesh
                .anchors
                .iter()
                .filter(|a| a.anchor_id != active.anchor_id)
                .map(format_anchor_resolved)
                .collect();
            let block = BasicOutput {
                active_anchor: active_anchor_str,
                mesh_name: mesh.name.clone(),
                why: mesh.message.clone(),
                status_if_not_fresh,
                non_active_anchors,
            };
            output.push_str(&block.to_string());
            emitted_meshes_this_call.push(mesh.name.clone());

            if !already_seen {
                new_meshes_seen.push(mesh.name.clone());
            }
            if !already_candidate {
                new_mesh_candidates.push(mesh.name.clone());
            }
            if !flags.has_printed_reconciliation_instructions {
                output.push_str(&reconciliation_instructions(mesh));
                flags.has_printed_reconciliation_instructions = true;
            }
        }
    }

    // Advance last-flush state (same as milestone Step 5).
    if last_flush_objects.exists() {
        std::fs::remove_dir_all(&last_flush_objects)
            .map_err(|e| anyhow::anyhow!("remove `{}`: {e}", last_flush_objects.display()))?;
    }
    // current_objects is still alive here (guard not yet dropped).
    std::fs::rename(&current_objects, &last_flush_objects).map_err(|e| {
        anyhow::anyhow!(
            "rename `{}` -> `{}`: {e}",
            current_objects.display(),
            last_flush_objects.display()
        )
    })?;
    // Disarm the guard since rename succeeded.
    std::mem::forget(_current_objects_guard);

    let new_cursor = store.reads_byte_len()?;
    let new_last_flush = crate::advice::session::state::BaselineState {
        schema_version: crate::advice::session::SCHEMA_VERSION,
        tree_sha: current.tree_sha.clone(),
        index_sha: baseline.index_sha.clone(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        read_cursor: new_cursor,
    };
    store.write_last_flush(&new_last_flush)?;

    // Build and persist touch intervals from incr_delta.
    let touch_ts = chrono::Utc::now().to_rfc3339();
    let touch_intervals = build_touch_intervals(
        incr_delta.as_slice(),
        &[],
        &touch_ts,
        &internal_path_prefixes,
    );
    for interval in &touch_intervals {
        store.append_touch(interval)?;
    }

    // ── Block A — touched-and-stale reconciliation sweep ─────────────────────
    // Collect meshes that overlap FILES_MODIFIED_DURING_SESSION, are not already
    // in meshes-seen or mesh-candidates, and are stale.
    let mut reconcile_meshes: Vec<&crate::types::MeshResolved> = Vec::new();

    for entry in &session_delta {
        let path = match entry {
            DiffEntry::Modified { path, .. } => path.clone(),
            DiffEntry::ModeChange { path, .. } => path.clone(),
            DiffEntry::Renamed { to, .. } => to.clone(),
            DiffEntry::Added { .. } => continue,
            DiffEntry::Deleted { .. } => continue,
        };
        if advice_path_is_internal(&path, &internal_path_prefixes) {
            continue;
        }
        let action = Action::WholeFile { path };

        for mesh in &meshes {
            // Already collected this mesh in this sweep.
            if reconcile_meshes.iter().any(|m| m.name == mesh.name) {
                continue;
            }
            // Already in meshes-seen.
            if meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name) {
                continue;
            }
            // Already in mesh-candidates.
            if mesh_candidates.contains(&mesh.name) || new_mesh_candidates.contains(&mesh.name) {
                continue;
            }
            // Must overlap the touched file.
            if !mesh.anchors.iter().any(|a| edit_overlaps(&action, a)) {
                continue;
            }
            // Must be stale.
            if !mesh_is_stale(mesh) {
                continue;
            }
            reconcile_meshes.push(mesh);
        }
    }

    if !reconcile_meshes.is_empty() {
        output.push_str("Reconcile the following meshes:\n");
        for mesh in &reconcile_meshes {
            // Emit BasicOutput for the first stale anchor as the active anchor.
            let active_anchor_resolved = mesh
                .anchors
                .iter()
                .find(|a| !matches!(a.status, crate::types::AnchorStatus::Fresh))
                .or_else(|| mesh.anchors.first());
            if let Some(active) = active_anchor_resolved {
                let active_anchor_str = format_anchor_resolved(active);
                let status_if_not_fresh =
                    if matches!(active.status, crate::types::AnchorStatus::Fresh) {
                        None
                    } else {
                        Some(Status::from_anchor_status(&active.status))
                    };
                let non_active_anchors: Vec<String> = mesh
                    .anchors
                    .iter()
                    .filter(|a| a.anchor_id != active.anchor_id)
                    .map(format_anchor_resolved)
                    .collect();
                let block = BasicOutput {
                    active_anchor: active_anchor_str,
                    mesh_name: mesh.name.clone(),
                    why: mesh.message.clone(),
                    status_if_not_fresh,
                    non_active_anchors,
                };
                output.push_str(&block.to_string());
            }

            new_meshes_seen.push(mesh.name.clone());
            new_mesh_candidates.push(mesh.name.clone());
        }

        // Reconciliation instructions: at most once per session.
        if !flags.has_printed_reconciliation_instructions
            && let Some(first) = reconcile_meshes.first()
        {
            output.push_str(&reconciliation_instructions(first));
            flags.has_printed_reconciliation_instructions = true;
        }
    }

    // ── Block B — related-anchors creation sweep ─────────────────────────────
    // new_fps is populated here but written to advice-seen AFTER stdout (F3).
    let mut block_b_new_fps: Vec<String> = Vec::new();
    {
        use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};

        let advice_dir = match std::env::var("GIT_MESH_ADVICE_DIR") {
            Ok(s) if !s.is_empty() => std::path::PathBuf::from(s),
            _ => store
                .dir()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        };
        let sessions = if advice_dir.as_os_str().is_empty() {
            Vec::new()
        } else {
            load_all_sessions(&advice_dir).unwrap_or_default()
        };

        if !sessions.is_empty() {
            let cfg = SuggestConfig::from_env();
            let suggestions = run_suggest_pipeline(&sessions, Some(repo), wd, &cfg);
            let advice_seen = store.advice_seen_set()?;

            let mut anchor_lines: Vec<String> = Vec::new();

            for sug in &suggestions {
                use crate::advice::suggestion::ConfidenceBand;
                if !matches!(sug.band, ConfidenceBand::High | ConfidenceBand::HighPlus) {
                    continue;
                }
                let fp = crate::advice::fingerprint::fingerprint_suggestion(sug);
                if advice_seen.contains(&fp) || block_b_new_fps.contains(&fp) {
                    continue;
                }
                block_b_new_fps.push(fp);

                for p in &sug.participants {
                    let anchor = if p.whole {
                        p.path.to_string_lossy().into_owned()
                    } else {
                        format!("{}#L{}-L{}", p.path.to_string_lossy(), p.start, p.end)
                    };
                    anchor_lines.push(anchor);
                }
            }

            if !block_b_new_fps.is_empty() {
                output.push_str(
                    "Use `git mesh` to document implicit semantic dependencies. Potential candidates:\n",
                );
                for line in &anchor_lines {
                    output.push_str(&format!("- {line}\n"));
                }

                if !flags.has_printed_creation_instructions {
                    let creation_text = creation_instructions(&[]);
                    output.push_str(&creation_text);
                    flags.has_printed_creation_instructions = true;
                }
            }
        }
    }

    // ── Write output ──────────────────────────────────────────────────────────
    use std::io::Write;
    let stdout_result = if output.is_empty() {
        Ok(())
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(output.as_bytes())
            .and_then(|()| handle.flush())
    };
    match stdout_result {
        Ok(()) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            return Err(anyhow::Error::from(e).context("write advice to stdout"));
        }
    }

    // Persist seen sets and flags only after successful output (F3: advice-seen
    // written after stdout, matching milestone's ordering).
    if !new_meshes_seen.is_empty() {
        store.append_meshes_seen(&new_meshes_seen)?;
    }
    if !new_mesh_candidates.is_empty() {
        store.append_mesh_candidates(&new_mesh_candidates)?;
    }
    if !block_b_new_fps.is_empty() {
        store.append_advice_seen(&block_b_new_fps)?;
    }

    // Step 8: persist SessionFlags.
    store.write_flags(&flags)?;

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

/// Reject `read` specs that point at non-existent paths or out-of-anchor
/// / inverted line ranges.
fn validate_read_spec(repo: &gix::Repository, spec: &str) -> Result<()> {
    if spec.is_empty() {
        bail!("invalid spec: path must not be empty");
    }
    let (path_str, anchor) = match spec.split_once("#L") {
        Some((p, frag)) => {
            let (s, e) = frag.split_once("-L").ok_or_else(|| {
                anyhow::anyhow!("invalid anchor `{spec}`; expected <path>#L<start>-L<end>")
            })?;
            let start: u32 = s
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid anchor start in `{spec}`"))?;
            let end: u32 = e
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid anchor end in `{spec}`"))?;
            if start < 1 {
                bail!("invalid anchor `{spec}`: start must be at least 1");
            }
            if end < start {
                bail!("invalid anchor `{spec}`: end ({end}) is before start ({start})");
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
    if let Some((start, end)) = anchor {
        let bytes = std::fs::read(&abs).map_err(|e| anyhow::anyhow!("read `{path_str}`: {e}"))?;
        let line_count = String::from_utf8_lossy(&bytes).lines().count() as u32;
        if end > line_count {
            bail!(
                "invalid anchor `{spec}`: end ({end}) is past EOF (extent has {line_count} lines)"
            );
        }
        let _ = start;
    }
    Ok(())
}
