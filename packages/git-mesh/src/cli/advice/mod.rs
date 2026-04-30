//! `git mesh advice` subcommand — per-`tool_use_id` snapshot/diff that
//! attributes working-tree changes to the tool call that caused them, plus
//! corpus-wide mesh suggester output for the `suggest` debug surface.

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
    /// Corpus-wide debug/parity surface for the n-ary mesh suggester.
    Suggest,
}

/// Top-level dispatch.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    if matches!(args.command, Some(AdviceCommand::Suggest)) {
        return run_advice_suggest();
    }
    let session_id = args.session_id.ok_or_else(|| {
        anyhow::anyhow!("git mesh advice: a <SESSION_ID> is required (e.g. `git mesh advice <id>`)")
    })?;
    validate_session_id(&session_id)?;
    match args.command {
        Some(AdviceCommand::Mark { id }) => run_advice_mark(repo, session_id, id),
        Some(AdviceCommand::Flush { id }) => run_advice_flush(repo, session_id, id),
        Some(AdviceCommand::Read { anchor, id }) => run_advice_read(repo, session_id, anchor, id),
        Some(AdviceCommand::Suggest) => unreachable!("handled above"),
        None => bail!(
            "git mesh advice: a subcommand is required; run `git mesh advice --help` for usage"
        ),
    }
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
    let mut out: Vec<String> = Vec::new();
    for (path, range) in paths {
        let names = crate::mesh::path_index::matching_mesh_names(repo, path, range)
            .unwrap_or_default();
        for name in names {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
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
    let _ = store.snapshots_dir()?;
    // Opportunistic orphan sweep (30 minute threshold) so a `mark` without
    // its `flush` doesn't accumulate forever.
    let _ = store.sweep_orphan_snapshots(std::time::Duration::from_secs(30 * 60));

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
    use crate::advice::session::state::{TouchInterval, TouchKind, UntrackedSnapshotEntry};
    use crate::advice::structured::{
        Action, BasicOutput, Status, creation_instructions, edit_overlaps, format_anchor_resolved,
        mesh_is_stale, reconciliation_instructions,
    };

    if id.is_empty() {
        bail!("git mesh advice <sid> flush <id>: id must not be empty");
    }
    let wd = work_dir(repo)?;
    let gd = repo.git_dir().to_path_buf();
    let store = SessionStore::open(wd, &gd, &session_id)?;

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

    let touches: Vec<TouchInterval> = entries
        .iter()
        .filter(|(p, _)| !advice_path_is_internal(p, &internal_path_prefixes))
        .map(|(path, kind)| TouchInterval {
            path: path.clone(),
            kind: *kind,
            id: id.clone(),
            ts: touch_ts.clone(),
        })
        .collect();

    let meshes = {
        let _perf = crate::perf::span("advice.flush.resolve-candidates");
        let candidate_names = candidate_mesh_names_for_paths(
            repo,
            touches
                .iter()
                .filter(|t| !matches!(t.kind, TouchKind::Added | TouchKind::Deleted))
                .map(|t| (t.path.as_str(), None)),
        );
        let resolved = crate::resolver::resolve_named_meshes(
            repo,
            &candidate_names,
            default_engine_options(),
        )
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

    for t in &touches {
        if matches!(t.kind, TouchKind::Added | TouchKind::Deleted) {
            continue;
        }
        let action = Action::WholeFile {
            path: t.path.clone(),
        };
        for mesh in &meshes {
            if emitted_meshes_this_call.contains(&mesh.name) {
                continue;
            }
            let Some(active) = mesh.anchors.iter().find(|a| edit_overlaps(&action, a)) else {
                continue;
            };
            let stale = mesh_is_stale(mesh);
            let already_seen =
                meshes_seen.contains(&mesh.name) || new_meshes_seen.contains(&mesh.name);
            if !stale && already_seen {
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
            if !already_seen {
                new_meshes_seen.push(mesh.name.clone());
            }
            if !new_mesh_candidates.contains(&mesh.name) {
                new_mesh_candidates.push(mesh.name.clone());
            }
            if !flags.has_printed_reconciliation_instructions {
                output.push_str(&reconciliation_instructions(mesh));
                flags.has_printed_reconciliation_instructions = true;
            }
        }
    }

    let added_paths: Vec<String> = touches
        .iter()
        .filter(|t| matches!(t.kind, TouchKind::Added))
        .map(|t| t.path.clone())
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
                if !matches!(sug.band, ConfidenceBand::High | ConfidenceBand::HighPlus) {
                    continue;
                }
                let fp = crate::advice::fingerprint::fingerprint_suggestion(sug);
                if advice_seen.contains(&fp) || emitted_fps.contains(&fp) {
                    continue;
                }
                let has_added_participant = sug.participants.iter().any(|p| {
                    added_paths
                        .iter()
                        .any(|ap| p.path.to_string_lossy().as_ref() == ap.as_str())
                });
                if !has_added_participant {
                    continue;
                }
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
            output.push_str(&creation_instructions(&[]));
            flags.has_printed_creation_instructions = true;
        }
    }

    for t in &touches {
        store.append_touch(t)?;
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
    store.discard_snapshot(&id);
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

    if anchor.is_empty() {
        bail!("git mesh advice <id> read: anchor must not be empty");
    }
    validate_read_spec(repo, &anchor)?;

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
        let candidate_names = candidate_mesh_names_for_paths(
            repo,
            std::iter::once((rec.path.as_str(), line_anchor)),
        );
        let resolved = crate::resolver::resolve_named_meshes(
            repo,
            &candidate_names,
            default_engine_options(),
        )
        .unwrap_or_default();
        resolved
            .into_iter()
            .filter_map(|(_, r)| r.ok())
            .collect::<Vec<_>>()
    };
    let meshes_seen = store.meshes_seen_set()?;

    let mut new_meshes_seen: Vec<String> = Vec::new();
    let mut new_mesh_candidates: Vec<String> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();

    for mesh in &meshes {
        let Some(active) = mesh.anchors.iter().find(|a| read_overlaps(&action, a)) else {
            continue;
        };
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

// ── suggest (corpus-wide) ───────────────────────────────────────────────────

pub fn run_advice_suggest_standalone() -> Result<i32> {
    run_advice_suggest()
}

fn run_advice_suggest() -> Result<i32> {
    use crate::advice::suggest::{SuggestConfig, run_suggest_pipeline};

    let advice_dir_str = std::env::var("GIT_MESH_ADVICE_DIR").unwrap_or_default();
    if advice_dir_str.is_empty() {
        bail!(
            "GIT_MESH_ADVICE_DIR is not set; the suggester is the parity surface \
             and requires a captured session corpus"
        );
    }
    let advice_dir = std::path::PathBuf::from(&advice_dir_str);
    if !advice_dir.exists() {
        bail!(
            "GIT_MESH_ADVICE_DIR points at a directory that does not exist: `{}`",
            advice_dir.display()
        );
    }
    let cfg = SuggestConfig::from_env();
    let sessions = load_all_sessions(&advice_dir)?;
    if sessions.is_empty() {
        bail!("{}", no_sessions_error_message(&advice_dir));
    }
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

fn load_all_sessions(dir: &std::path::Path) -> Result<Vec<crate::advice::suggest::SessionRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let fixture_mode = std::env::var("GIT_MESH_SUGGEST_FIXTURE").as_deref() == Ok("1");
    let preferred_key: Option<String> = gix::discover(".").ok().and_then(|repo| {
        let root = repo.workdir().map(|p| p.to_path_buf())?;
        let git_dir = repo.git_dir().to_path_buf();
        Some(crate::advice::session::store::repo_key(&root, &git_dir))
    });
    let cross_corpus = fixture_mode || preferred_key.is_none();

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

    let mut flat_dirs: Vec<std::path::PathBuf> = Vec::new();
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
            (true, false) => flat_dirs.push(entry),
            (false, true) => {
                let key = entry
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                keyed_sessions.entry(key).or_default().extend(nested);
            }
            (true, true) => {
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
            (false, false) => {}
        }
    }
    let mut chosen_dirs: Vec<std::path::PathBuf> = Vec::new();
    if cross_corpus {
        chosen_dirs.extend(flat_dirs);
        for (_, dirs) in keyed_sessions {
            chosen_dirs.extend(dirs);
        }
    } else {
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
    }
    chosen_dirs.sort();
    sessions_from_dirs(chosen_dirs)
}

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
