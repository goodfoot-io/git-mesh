//! `git mesh advice` subcommand — per-`tool_use_id` snapshot/diff that
//! attributes working-tree changes to the tool call that caused them.

#[cfg(test)]
mod tests;

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::git::work_dir;

const SESSION_ID_RULE: &str = "non-empty; ASCII letters, digits, `-`, `_`, and `.`; \
     no `/`, no `\\`, no NUL, no whitespace or other control characters";

#[derive(Debug, clap::Args)]
pub struct AdviceArgs {
    /// Session identifier. See [`SESSION_ID_RULE`].
    pub session_id: Option<String>,

    #[command(subcommand)]
    pub command: Option<AdviceCommand>,
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Capture a per-`<id>` working-tree snapshot pair (saved index + saved
    /// untracked manifest) for a tool call about to start.
    Mark {
        /// Caller-chosen opaque id (typically `tool_use_id`).
        id: String,
    },
    /// Diff against the snapshot pair captured by `mark <id>`, append the
    /// resulting per-path entries to `touches.jsonl`, emit mesh suggestions
    /// for newly-touched paths (deduped against `meshes-seen.jsonl`) on
    /// stdout, and discard the snapshot. A no-op when no snapshot exists
    /// for `<id>`.
    Flush {
        /// Caller-chosen opaque id matching the `mark`.
        id: String,
    },
    /// Record a single read event (anchor or whole-file path).
    Read {
        /// Anchor to record. Either `<path>` or `<path>#L<start>-L<end>`.
        anchor: String,
        /// Optional caller-chosen opaque id correlating this read with a
        /// `mark`/`flush` pair.
        id: Option<String>,
    },
    /// Record a single payload-driven touch from an Edit/Write/MultiEdit call,
    /// bypassing the snapshot path.
    Touch {
        /// Caller-chosen opaque id (typically `tool_use_id`).
        id: String,
        /// Anchor: either `<path>` (whole-file) or `<path>#L<start>-L<end>`.
        anchor: String,
        /// Kind of change: `added` or `modified`.
        kind: TouchKindArg,
    },
    /// Remove the session directory and all snapshot artefacts. Best-effort
    /// and idempotent — missing directory is not an error.
    End,
    /// List repo-relative paths created, updated, or deleted during this
    /// session. Read-only: does not modify any session state.
    Touched,
}

/// Clap-derive enum for the `touch` verb's `kind` argument.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum TouchKindArg {
    Added,
    Modified,
}

/// Top-level dispatch.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    let session_id = args.session_id.ok_or_else(|| {
        anyhow::anyhow!("git mesh advice: a <SESSION_ID> is required (e.g. `git mesh advice <id>`)")
    })?;
    validate_session_id(&session_id)?;
    match args.command {
        Some(AdviceCommand::Mark { id }) => run_advice_mark(repo, session_id, id),
        Some(AdviceCommand::Flush { id }) => run_advice_flush(repo, session_id, id),
        Some(AdviceCommand::Read { anchor, id }) => run_advice_read(repo, session_id, anchor, id),
        Some(AdviceCommand::Touch { id, anchor, kind }) => {
            run_advice_touch(repo, session_id, id, anchor, kind)
        }
        Some(AdviceCommand::End) => run_advice_end(repo, session_id),
        Some(AdviceCommand::Touched) => run_advice_touched(repo, session_id),
        None => bail!(
            "git mesh advice: a subcommand is required; run `git mesh advice --help` for usage"
        ),
    }
}

// ── Path normalization ────────────────────────────────────────────────────────

/// Normalize a raw repo-relative path string to a canonical form suitable for
/// consistent comparison across `reads.jsonl` and `pending_touches.jsonl`.
///
/// Steps:
/// 1. Strip a leading `./` prefix (produced by some callers defensively).
/// 2. Join against the working directory and call `fs::canonicalize` if the
///    path exists; otherwise apply lexical normalization (resolve `..`
///    components without touching the filesystem).
/// 3. Strip the canonical working-directory prefix to produce a clean
///    repo-relative path in forward-slash form.
/// 4. On case-insensitive targets (macOS, Windows), lowercase the result so
///    reads and touches always agree regardless of case.
///
/// Returns `Err` only when the resulting path escapes the working directory.
pub(crate) fn canonicalize_repo_relative_path(wd: &std::path::Path, raw: &str) -> Result<String> {
    // Strip leading `./`.
    let stripped = raw.strip_prefix("./").unwrap_or(raw);

    let joined = wd.join(stripped);

    // Attempt real canonicalization when the file exists so symlinks are
    // resolved. When it doesn't exist, canonicalize the deepest existing
    // ancestor and reattach the missing tail so symlinks in the wd prefix
    // (e.g. /var → /private/var on macOS) are still resolved symmetrically
    // with the canonical_wd computed below.
    let canonical = if joined.exists() {
        std::fs::canonicalize(&joined)
            .unwrap_or_else(|_| lexical_normalize(&joined))
    } else {
        // Walk up until we find an ancestor that exists on disk, then
        // canonicalize it and reattach the unresolved tail.
        let mut ancestor: &std::path::Path = &joined;
        loop {
            match ancestor.parent() {
                Some(p) => {
                    ancestor = p;
                    if ancestor.exists() {
                        let tail = joined.strip_prefix(ancestor).unwrap_or(&joined);
                        let canonical_ancestor = std::fs::canonicalize(ancestor)
                            .unwrap_or_else(|_| ancestor.to_path_buf());
                        break lexical_normalize(&canonical_ancestor.join(tail));
                    }
                }
                None => break lexical_normalize(&joined),
            }
        }
    };

    // Also canonicalize wd so the prefix strip is symmetric.
    let canonical_wd = std::fs::canonicalize(wd).unwrap_or_else(|_| wd.to_path_buf());

    let rel = canonical
        .strip_prefix(&canonical_wd)
        .map_err(|_| {
            anyhow::anyhow!(
                "path `{raw}` resolves outside the working directory `{}`",
                wd.display()
            )
        })?;

    let result = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");

    // Case-fold on case-insensitive targets.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let result = result.to_lowercase();

    Ok(result)
}

/// Lexically normalize `path` by resolving `.` and `..` components without
/// calling `fs::canonicalize` (which requires the path to exist).
fn lexical_normalize(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop();
            }
            other => {
                stack.push(other.as_os_str().to_owned());
            }
        }
    }
    stack.iter().collect()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn advice_path_is_internal(path: &str, internal_path_prefixes: &[String]) -> bool {
    internal_path_prefixes.iter().any(|prefix| {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
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

/// Build a deduped, order-preserving list of candidate mesh names for the
/// given `(path, optional line range)` pairs, using the path index. Errors
/// from individual path-index reads are skipped so a single bad bucket cannot
/// break advice rendering.
fn candidate_mesh_names_for_paths<'a, I>(repo: &gix::Repository, paths: I) -> Vec<String>
where
    I: IntoIterator<Item = (&'a str, Option<(u32, u32)>)>,
{
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for (path, range) in paths {
        let names =
            crate::mesh::path_index::matching_mesh_names(repo, path, range).unwrap_or_default();
        for name in names {
            if seen.insert(name.clone()) {
                out.push(name);
            }
        }
    }
    out
}

/// Discover meshes whose ref OID differs from the session baseline.
/// Names not in the baseline, or with a changed OID, are appended to
/// `meshes-committed.jsonl` (unless already committed this session).
/// Returns the full committed set (including any newly appended names).
///
/// In `process_touches` (flush path), errors are swallowed via `let _ =`
/// because observation is best-effort there. In `run_advice_read`, errors
/// propagate via `?` to fail-closed (no advice emitted).
fn discover_meshes_committed_this_session(
    repo: &gix::Repository,
    store: &crate::advice::session::SessionStore,
) -> Result<std::collections::HashSet<String>> {
    let baseline = store.mesh_baseline_map()?;
    let mut committed = store.meshes_committed_set()?;
    let current = crate::mesh::read::list_mesh_refs(repo)?;
    let mut new_names: Vec<String> = Vec::new();
    for (name, oid) in &current {
        let is_new = match baseline.get(name) {
            Some(prior_oid) => prior_oid != oid,
            None => true,
        };
        if is_new && !committed.contains(name) {
            new_names.push(name.clone());
        }
    }
    if !new_names.is_empty() {
        store.append_meshes_committed(&new_names)?;
        for n in &new_names {
            committed.insert(n.clone());
        }
    }
    Ok(committed)
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

// ── mark ────────────────────────────────────────────────────────────────────

fn run_advice_mark(repo: &gix::Repository, session_id: String, id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::UntrackedSnapshotEntry;

    if id.is_empty() {
        bail!("git mesh advice <sid> mark <id>: id must not be empty");
    }
    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;
    store.ensure_initialized()?;
    store.ensure_mesh_baseline(repo)?;
    let _ = store.snapshots_dir()?;
    // Opportunistic orphan sweep (30 minute threshold) so a `mark` without
    // its `flush` doesn't accumulate forever.
    let _ = store.sweep_orphan_snapshots(std::time::Duration::from_secs(30 * 60));
    // Sweep pending touches on the same cycle so long sessions don't grow
    // pending_touches.jsonl unboundedly.
    let _ = store.sweep_pending_touches(std::time::Duration::from_secs(30 * 60));

    let index_src = gd.join("index");
    let index_dst = store.snapshot_index_path(&id);
    match std::fs::copy(&index_src, &index_dst) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&index_dst, b"")?;
        }
        Err(e) => return Err(anyhow::Error::from(e).context("copy .git/index")),
    }

    let untracked = ls_files_untracked(wd)?;
    let mut entries: Vec<UntrackedSnapshotEntry> = Vec::with_capacity(untracked.len());
    for path in untracked {
        let abs = wd.join(&path);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(_) => continue,
        };
        entries.push(untracked_entry_from_meta(&path, &meta));
    }

    let untracked_path = store.snapshot_untracked_path(&id);
    let tmp = untracked_path.with_extension("untracked.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        for e in &entries {
            let line = serde_json::to_string(e)?;
            writeln!(f, "{line}")?;
        }
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &untracked_path)?;
    Ok(0)
}

#[cfg(unix)]
fn untracked_entry_from_meta(
    path: &str,
    meta: &std::fs::Metadata,
) -> crate::advice::session::state::UntrackedSnapshotEntry {
    use std::os::unix::fs::MetadataExt;
    crate::advice::session::state::UntrackedSnapshotEntry {
        path: path.to_string(),
        size: meta.len(),
        mode: meta.mode(),
        mtime_ns: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        ctime_ns: meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
        ino: meta.ino(),
    }
}

#[cfg(not(unix))]
fn untracked_entry_from_meta(
    path: &str,
    meta: &std::fs::Metadata,
) -> crate::advice::session::state::UntrackedSnapshotEntry {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    crate::advice::session::state::UntrackedSnapshotEntry {
        path: path.to_string(),
        size: meta.len(),
        mode: 0,
        mtime_ns,
        ctime_ns: mtime_ns,
        ino: 0,
    }
}

fn ls_files_untracked(wd: &std::path::Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .current_dir(wd)
        .args(["ls-files", "-z", "-o", "--exclude-standard"])
        .output()?;
    if !out.status.success() {
        bail!(
            "git ls-files -o failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

// ── flush ───────────────────────────────────────────────────────────────────

fn run_advice_flush(repo: &gix::Repository, session_id: String, id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::{TouchInterval, UntrackedSnapshotEntry};

    if id.is_empty() {
        bail!("git mesh advice <sid> flush <id>: id must not be empty");
    }
    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;
    store.ensure_initialized()?;
    store.ensure_mesh_baseline(repo)?;

    if !store.snapshot_exists(&id) {
        return Ok(0);
    }
    let saved_index = store.snapshot_index_path(&id);
    let saved_untracked_path = store.snapshot_untracked_path(&id);

    let saved_untracked: std::collections::HashMap<String, UntrackedSnapshotEntry> =
        load_untracked_map(&saved_untracked_path)?;

    let entries = diff_against_saved(wd, &saved_index, &saved_untracked)?;
    let internal_path_prefixes = active_advice_store_prefixes(wd, store.dir());
    let touch_ts = chrono::Utc::now().to_rfc3339();

    let all_touches: Vec<TouchInterval> = entries
        .iter()
        .filter(|(p, _)| !advice_path_is_internal(p, &internal_path_prefixes))
        .filter_map(|(path, kind)| {
            // Canonicalize so flush paths agree with read paths on all
            // case/symlink/leading-./ variants. Skip entries that escape wd.
            let canonical_path = canonicalize_repo_relative_path(wd, path).ok()?;
            Some(TouchInterval {
                path: canonical_path,
                kind: *kind,
                id: id.clone(),
                ts: touch_ts.clone(),
                start: None,
                end: None,
            })
        })
        .collect();

    process_touches(repo, &store, &session_id, &id, all_touches)
}

/// Shared mesh-resolution and emission pipeline. Called by both `flush`
/// (snapshot-derived touches) and `touch` (payload-driven touches).
/// Always calls `store.discard_snapshot(id)` at the end — this is a no-op
/// when no snapshot was taken (the snapshot files do not exist).
fn process_touches(
    repo: &gix::Repository,
    store: &crate::advice::session::SessionStore,
    session_id: &str,
    id: &str,
    touches: Vec<crate::advice::session::state::TouchInterval>,
) -> Result<i32> {
    use crate::advice::session::state::TouchKind;
    use crate::advice::structured::{
        Action, BasicOutput, Status, edit_overlaps, format_anchor_resolved,
        reconciliation_instructions,
    };

    let wd = work_dir(repo)?;

    let meshes = {
        let _perf = crate::perf::span("advice.flush.resolve-candidates");
        let candidate_names = candidate_mesh_names_for_paths(
            repo,
            touches
                .iter()
                .filter(|t| !matches!(t.kind, TouchKind::Added | TouchKind::Deleted))
                .map(|t| {
                    let range = match (t.start, t.end) {
                        (Some(s), Some(e)) => Some((s, e)),
                        _ => None,
                    };
                    (t.path.as_str(), range)
                }),
        );
        let resolved =
            crate::resolver::resolve_named_meshes(repo, &candidate_names, default_engine_options())
                .unwrap_or_default();
        resolved
            .into_iter()
            .filter_map(|(_, r)| r.ok())
            .collect::<Vec<_>>()
    };
    let meshes_seen = store.meshes_seen_set()?;
    let mut flags = store.read_flags()?;

    let mut output = String::new();
    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();
    let mut emitted_meshes_this_call: Vec<String> = Vec::new();

    // Precompute, per candidate mesh, a path → anchors index so the per-touch
    // emission loop runs an O(1) lookup keyed on `t.path` and only re-checks
    // overlap against path-matching anchors. Without this, each (touch, mesh)
    // pair walked the mesh's full anchor list — which becomes the dominant
    // cost on a tool call that mass-edits many files.
    let mesh_anchor_index: Vec<
        std::collections::HashMap<String, Vec<&crate::types::AnchorResolved>>,
    > = meshes
        .iter()
        .map(|mesh| {
            let mut map: std::collections::HashMap<String, Vec<&crate::types::AnchorResolved>> =
                std::collections::HashMap::new();
            for a in &mesh.anchors {
                let key = a.anchored.path.to_string_lossy().into_owned();
                map.entry(key).or_default().push(a);
            }
            map
        })
        .collect();

    for t in &touches {
        if matches!(t.kind, TouchKind::Added | TouchKind::Deleted) {
            continue;
        }
        let action = if let (Some(start), Some(end)) = (t.start, t.end) {
            Action::Range {
                path: t.path.clone(),
                start,
                end,
            }
        } else {
            Action::WholeFile {
                path: t.path.clone(),
            }
        };
        for (mesh, anchor_index) in meshes.iter().zip(mesh_anchor_index.iter()) {
            if emitted_meshes_this_call.contains(&mesh.name) {
                continue;
            }
            let Some(path_anchors) = anchor_index.get(&t.path) else {
                continue;
            };
            let Some(active) = path_anchors
                .iter()
                .copied()
                .find(|a| edit_overlaps(&action, a))
            else {
                continue;
            };
            if meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name) {
                continue;
            }
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
            new_meshes_seen.push(mesh.name.clone());
            if !new_mesh_candidates.contains(&mesh.name) {
                new_mesh_candidates.push(mesh.name.clone());
            }
            if !flags.has_printed_reconciliation_instructions {
                output.push_str(&reconciliation_instructions(mesh));
                flags.has_printed_reconciliation_instructions = true;
            }
        }
    }

    // Persist the current flush's touches BEFORE building the SessionRecord
    // for the suggester. The pipeline reads `store.load_touches()` to seed the
    // single active session; if we appended after the suggester ran, a flush
    // triggered by a touch (no prior `advice_mark`) would arrive at the
    // pipeline with an empty seed and produce no output. Persisting first
    // closes that gap; the gate downstream still reads `&touches` directly so
    // we do not depend on a re-read for "current-flush" semantics.
    for t in &touches {
        store.append_touch(t)?;
    }

    let deleted_paths: std::collections::HashSet<String> = touches
        .iter()
        .filter(|t| matches!(t.kind, TouchKind::Deleted))
        .map(|t| t.path.clone())
        .collect();
    let turn_reads: Vec<String> = store
        .load_reads()?
        .into_iter()
        .filter(|r| r.id.as_deref() == Some(id))
        .map(|r| r.path)
        .collect();
    let gate_seed: Vec<String> = {
        let mut s: Vec<String> = touches
            .iter()
            .filter(|t| !matches!(t.kind, TouchKind::Deleted))
            .map(|t| t.path.clone())
            .collect();
        for p in &turn_reads {
            if !s.contains(p) {
                s.push(p.clone());
            }
        }
        s
    };
    let mut emitted_fps: Vec<String> = Vec::new();

    if !gate_seed.is_empty() {
        use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};
        let advice_dir = match std::env::var("GIT_MESH_ADVICE_DIR") {
            Ok(s) if !s.is_empty() => std::path::PathBuf::from(s),
            _ => store
                .dir()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        };
        let session_record = if advice_dir.as_os_str().is_empty() {
            None
        } else {
            use crate::advice::suggest::SessionRecord;
            match (store.load_reads(), store.load_touches()) {
                (Ok(reads), Ok(touches)) => Some(SessionRecord {
                    sid: session_id.to_string(),
                    reads,
                    touches,
                }),
                _ => None,
            }
        };
        let sessions_buf;
        let sessions: &[_] = if let Some(ref rec) = session_record {
            sessions_buf = std::slice::from_ref(rec);
            sessions_buf
        } else {
            &[]
        };
        let mut creation_stanzas: Vec<String> = Vec::new();
        if !sessions.is_empty() {
            let cfg = SuggestConfig::from_env();
            let suggestions = run_suggest_pipeline(sessions, Some(repo), wd, &cfg, Some(store.dir()));
            let advice_seen = store.advice_seen_set()?;
            for sug in &suggestions {
                use crate::advice::suggestion::ConfidenceBand;
                if !matches!(sug.band, ConfidenceBand::High | ConfidenceBand::HighPlus) {
                    continue;
                }
                let fp = crate::advice::fingerprint::fingerprint_suggestion(sug);
                if advice_seen.contains(&fp) || emitted_fps.contains(&fp) {
                    continue;
                }
                let has_deleted_participant = sug.participants.iter().any(|p| {
                    deleted_paths.contains(p.path.to_string_lossy().as_ref())
                });
                if has_deleted_participant {
                    continue;
                }
                if any_participant_references_another(&sug.participants, wd) {
                    continue;
                }
                let anchors: Vec<String> = sug
                    .participants
                    .iter()
                    .map(|p| {
                        if p.whole {
                            p.path.to_string_lossy().into_owned()
                        } else {
                            format!("{}#L{}-L{}", p.path.to_string_lossy(), p.start, p.end)
                        }
                    })
                    .collect();
                let mut stanza = String::new();
                stanza.push_str("Detected possible implicit semantic dependency between:\n");
                for a in &anchors {
                    stanza.push_str(&format!("  - {a}\n"));
                }
                stanza.push_str("\nRecord the link if real:\n");
                stanza.push_str("  git mesh add <name> \\\n");
                let last = anchors.len() - 1;
                for (i, anchor) in anchors.iter().enumerate() {
                    if i == last {
                        stanza.push_str(&format!("    {anchor}\n"));
                    } else {
                        stanza.push_str(&format!("    {anchor} \\\n"));
                    }
                }
                stanza.push_str("  git mesh why <name> -m \"What these anchors do together.\"\n");
                creation_stanzas.push(stanza);
                emitted_fps.push(fp);
            }
        }
        for (i, stanza) in creation_stanzas.iter().enumerate() {
            if i > 0 {
                output.push_str("\n---\n\n");
            }
            output.push_str(stanza);
        }
    }

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

    if !new_meshes_seen.is_empty() {
        store.append_meshes_seen(&new_meshes_seen)?;
    }
    if !new_mesh_candidates.is_empty() {
        store.append_mesh_candidates(&new_mesh_candidates)?;
    }
    if !emitted_fps.is_empty() {
        store.append_advice_seen(&emitted_fps)?;
    }
    store.write_flags(&flags)?;
    // Eager observation: capture meshes committed via this tool call's
    // `git commit` so subsequent reads hit the fast `meshes_committed_set()` path.
    let _ = discover_meshes_committed_this_session(repo, store);
    store.discard_snapshot(id);
    Ok(0)
}

/// True iff any pair of distinct participants has one whose file content
/// contains the other's repo-relative path as a substring. This catches the
/// "explicit semantic dependency" case (e.g. one markdown file inline-links to
/// another) so we don't surface a coupling that is already documented in-tree.
///
/// Files that can't be read (missing, IO error, non-UTF-8) are treated as
/// empty — fail-open: emit the suggestion rather than silently swallow it.
fn any_participant_references_another(
    participants: &[crate::advice::candidates::MeshAnchor],
    wd: &std::path::Path,
) -> bool {
    let paths: Vec<String> = participants
        .iter()
        .map(|p| p.path.to_string_lossy().into_owned())
        .collect();
    let contents: Vec<String> = paths
        .iter()
        .map(|p| std::fs::read_to_string(wd.join(p)).unwrap_or_default())
        .collect();
    for (i, content) in contents.iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        for (j, path) in paths.iter().enumerate() {
            if i == j {
                continue;
            }
            if content.contains(path.as_str()) {
                return true;
            }
        }
    }
    false
}

// ── touch ───────────────────────────────────────────────────────────────────

fn run_advice_touch(
    repo: &gix::Repository,
    session_id: String,
    id: String,
    anchor: String,
    kind: TouchKindArg,
) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::{TouchInterval, TouchKind};

    if id.is_empty() {
        bail!("git mesh advice <sid> touch <id>: id must not be empty");
    }
    if anchor.is_empty() {
        bail!("git mesh advice <sid> touch: anchor must not be empty");
    }

    // Validate the anchor. For Added, skip the EOF check since the file may
    // be brand new. For Modified, reuse validate_read_spec which checks EOF.
    match kind {
        TouchKindArg::Modified => validate_read_spec(repo, &anchor)?,
        TouchKindArg::Added => validate_touch_anchor_added(repo, &anchor)?,
    }

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;
    store.ensure_initialized()?;
    store.ensure_mesh_baseline(repo)?;

    // Parse anchor into (path, Option<(start, end)>).
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

    let touch_kind = match kind {
        TouchKindArg::Added => TouchKind::Added,
        TouchKindArg::Modified => TouchKind::Modified,
    };

    let touches = vec![TouchInterval {
        path: path_str,
        kind: touch_kind,
        id,
        ts: chrono::Utc::now().to_rfc3339(),
        start: line_anchor.map(|(s, _)| s),
        end: line_anchor.map(|(_, e)| e),
    }];

    process_touches(repo, &store, &session_id, "", touches)
}

/// Validate an anchor for the `touch added` case. Same as `validate_read_spec`
/// but skips the EOF check because the file content may be brand new.
fn validate_touch_anchor_added(repo: &gix::Repository, spec: &str) -> Result<()> {
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
    let _ = anchor;
    Ok(())
}

// ── end ─────────────────────────────────────────────────────────────────────

fn run_advice_end(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::store::{advice_base_dir, repo_key};

    // Resolve session dir without opening/locking it. We can't use
    // SessionStore::open because that creates the dir and takes a lock —
    // both inappropriate for an idempotent cleanup.
    let wd = match repo.workdir() {
        Some(p) => p.to_path_buf(),
        None => return Ok(0),
    };
    let git_dir = repo.git_dir().to_path_buf();
    let session_dir = advice_base_dir()
        .join(repo_key(&wd, &git_dir))
        .join(&session_id);

    if !session_dir.exists() {
        return Ok(0);
    }

    // Sweep all snapshots unconditionally before removing the directory.
    // We do this manually to avoid needing a SessionStore (which would lock).
    let snapshots_dir = session_dir.join("snapshots");
    if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let _ = std::fs::remove_dir_all(&session_dir);
    Ok(0)
}

fn load_untracked_map(
    path: &std::path::Path,
) -> Result<std::collections::HashMap<String, crate::advice::session::state::UntrackedSnapshotEntry>>
{
    use crate::advice::session::state::UntrackedSnapshotEntry;
    use std::io::BufRead;
    let mut out = std::collections::HashMap::new();
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(anyhow::Error::from(e).context("open untracked snapshot")),
    };
    for line in std::io::BufReader::new(f).lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let entry: UntrackedSnapshotEntry = serde_json::from_str(&line)?;
        out.insert(entry.path.clone(), entry);
    }
    Ok(out)
}

fn diff_against_saved(
    wd: &std::path::Path,
    saved_index: &std::path::Path,
    saved_untracked: &std::collections::HashMap<
        String,
        crate::advice::session::state::UntrackedSnapshotEntry,
    >,
) -> Result<Vec<(String, crate::advice::session::state::TouchKind)>> {
    use crate::advice::session::state::TouchKind;
    let mut out: Vec<(String, TouchKind)> = Vec::new();

    let diff = std::process::Command::new("git")
        .current_dir(wd)
        .env("GIT_INDEX_FILE", saved_index)
        .args(["diff-files", "-z", "--raw", "--no-renames"])
        .output()?;
    if !diff.status.success() {
        bail!(
            "git diff-files failed: {}",
            String::from_utf8_lossy(&diff.stderr)
        );
    }
    parse_diff_files_z(&diff.stdout, &mut out);

    let mut current_map: std::collections::HashMap<String, std::fs::Metadata> =
        std::collections::HashMap::new();
    for path in ls_files_untracked(wd)? {
        let abs = wd.join(&path);
        if let Ok(m) = std::fs::symlink_metadata(&abs) {
            current_map.insert(path, m);
        }
    }
    for (path, meta) in &current_map {
        match saved_untracked.get(path) {
            None => out.push((path.clone(), TouchKind::Added)),
            Some(prev) => {
                let now = untracked_entry_from_meta(path, meta);
                if prev.size != now.size
                    || prev.mtime_ns != now.mtime_ns
                    || prev.ctime_ns != now.ctime_ns
                    || prev.ino != now.ino
                {
                    out.push((path.clone(), TouchKind::Modified));
                }
            }
        }
    }
    for path in saved_untracked.keys() {
        if !current_map.contains_key(path) {
            out.push((path.clone(), TouchKind::Deleted));
        }
    }
    Ok(out)
}

fn parse_diff_files_z(
    bytes: &[u8],
    out: &mut Vec<(String, crate::advice::session::state::TouchKind)>,
) {
    use crate::advice::session::state::TouchKind;
    // With -z, diff-files emits NUL-separated fields. Each entry is two
    // NUL-terminated chunks: ":<src_mode> <dst_mode> <src_sha> <dst_sha> <status>\0<path>\0"
    let mut chunks = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let Some(header) = chunks.next() {
        if header.is_empty() || header[0] != b':' {
            continue;
        }
        let Some(path_bytes) = chunks.next() else {
            break;
        };
        let header_str = String::from_utf8_lossy(&header[1..]);
        let mut fields = header_str.split(' ');
        let src_mode = fields.next().unwrap_or("");
        let dst_mode = fields.next().unwrap_or("");
        let _src_sha = fields.next().unwrap_or("");
        let _dst_sha = fields.next().unwrap_or("");
        let status = fields.next().unwrap_or("");
        let kind = match status.chars().next().unwrap_or(' ') {
            'D' => TouchKind::Deleted,
            'A' => TouchKind::Added,
            'M' => {
                if src_mode != dst_mode {
                    TouchKind::ModeChange
                } else {
                    TouchKind::Modified
                }
            }
            'T' => TouchKind::ModeChange,
            _ => TouchKind::Modified,
        };
        let path = String::from_utf8_lossy(path_bytes).into_owned();
        out.push((path, kind));
    }
}

// ── read ────────────────────────────────────────────────────────────────────

fn run_advice_read(
    repo: &gix::Repository,
    session_id: String,
    anchor: String,
    id: Option<String>,
) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::ReadRecord;
    use crate::advice::session::store::LockTimeout;
    use crate::advice::structured::{
        BasicOutput, Status, action_from_spec, format_anchor_resolved, read_overlaps,
    };

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;
    store.ensure_initialized()?;
    store.ensure_mesh_baseline(repo)?;

    if anchor.is_empty() {
        bail!("git mesh advice <id> read: anchor must not be empty");
    }
    validate_read_spec(repo, &anchor)?;

    let now = chrono::Utc::now().to_rfc3339();
    let (path_raw, line_anchor) = match anchor.split_once("#L") {
        Some((p, frag)) => {
            let (s, e) = frag.split_once("-L").unwrap();
            (
                p.to_string(),
                Some((s.parse::<u32>().unwrap(), e.parse::<u32>().unwrap())),
            )
        }
        None => (anchor.clone(), None),
    };
    let path_str = canonicalize_repo_relative_path(wd, &path_raw)?;
    let rec = ReadRecord {
        path: path_str,
        start_line: line_anchor.map(|(s, _)| s),
        end_line: line_anchor.map(|(_, e)| e),
        ts: now,
        id,
    };
    store.append_read(
        &rec,
        LockTimeout::Bounded(std::time::Duration::from_secs(30)),
    )?;

let action = action_from_spec(&anchor).ok_or_else(|| {
        anyhow::anyhow!("internal: action_from_spec returned None for `{anchor}`")
    })?;
    let meshes = {
        let _perf = crate::perf::span("advice.read.resolve-candidates");
        let candidate_names =
            candidate_mesh_names_for_paths(repo, std::iter::once((rec.path.as_str(), line_anchor)));
        let resolved =
            crate::resolver::resolve_named_meshes(repo, &candidate_names, default_engine_options())
                .unwrap_or_default();
        resolved
            .into_iter()
            .filter_map(|(_, r)| r.ok())
            .collect::<Vec<_>>()
    };
    let meshes_committed = discover_meshes_committed_this_session(repo, &store)?;
    let meshes_seen = store.meshes_seen_set()?;

    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();

    for mesh in &meshes {
        // Step 1: overlap check
        let Some(active) = mesh.anchors.iter().find(|a| read_overlaps(&action, a)) else {
            continue;
        };
        // Record ALL overlapping meshes as candidates BEFORE the same-session
        // filter. mesh-candidates.jsonl is an operational log of which meshes
        // were considered, not which were emitted.
        new_mesh_candidates.push(mesh.name.clone());
        // Step 2: same-session filter (NEW)
        if !meshes_committed.contains(&mesh.name) {
            continue;
        }
        // Step 3: meshes_seen dedup
        if meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name) {
            continue;
        }
        let active_anchor_str = format_anchor_resolved(active);
        let status_if_not_fresh = if matches!(active.status, crate::types::AnchorStatus::Fresh) {
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
        blocks.push(block.to_string());
        new_meshes_seen.push(mesh.name.clone());
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
            Err(e) => return Err(anyhow::Error::from(e).context("write advice to stdout")),
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

// ── touched ─────────────────────────────────────────────────────────────────

fn run_advice_touched(repo: &gix::Repository, session_id: String) -> Result<i32> {
    use crate::advice::session::SessionStore;
    use std::io::Write;

    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;

    let paths = collect_touched_paths(&store.dir().join("touches.jsonl"))?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let result = (|| -> std::io::Result<()> {
        for path in &paths {
            handle.write_all(path.as_bytes())?;
            handle.write_all(b"\n")?;
        }
        handle.flush()
    })();
    match result {
        Ok(()) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => return Err(anyhow::Error::from(e).context("write touched paths to stdout")),
    }
    Ok(0)
}

pub(crate) fn collect_touched_paths(touches_path: &std::path::Path) -> Result<Vec<String>> {
    use crate::advice::session::state::{TouchInterval, TouchKind};
    use std::io::BufRead;

    let f = match std::fs::File::open(touches_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!("open `{}`", touches_path.display())));
        }
    };

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut order: Vec<String> = Vec::new();
    for (idx, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| anyhow::anyhow!("read `{}`: {e}", touches_path.display()))?;
        if line.is_empty() {
            continue;
        }
        let t: TouchInterval = serde_json::from_str(&line).map_err(|e| {
            anyhow::anyhow!("parse `{}` line {}: {e}", touches_path.display(), idx + 1)
        })?;
        if matches!(t.kind, TouchKind::ModeChange) {
            continue;
        }
        if seen.insert(t.path.clone()) {
            order.push(t.path);
        }
    }
    Ok(order)
}

// ── validation ───────────────────────────────────────────────────────────────

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
