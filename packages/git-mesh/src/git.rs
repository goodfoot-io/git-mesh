//! Git plumbing helpers.
//!
//! Thin typed wrappers around `gix`. These are the only place in the
//! crate that talks to git directly; the rest of the crate stays on
//! typed results via [`crate::Result`].

use crate::{Error, Result};
use std::path::Path;
use std::str::FromStr;

use gix::ObjectId;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

// ---------------------------------------------------------------------------
// Ref transactions (ported from v1 legacy).
// ---------------------------------------------------------------------------

/// A single update in a `git update-ref --stdin` transaction.
pub(crate) enum RefUpdate {
    Create {
        name: String,
        new_oid: String,
    },
    Update {
        name: String,
        new_oid: String,
        expected_old_oid: String,
    },
    Delete {
        name: String,
        expected_old_oid: String,
    },
}

pub(crate) fn apply_ref_transaction(work_dir: &Path, updates: &[RefUpdate]) -> Result<()> {
    let repo = gix::open(work_dir).map_err(|e| Error::Git(format!("open repo: {e}")))?;
    apply_ref_transaction_repo(&repo, updates)
}

fn parse_oid(hex: &str) -> Result<ObjectId> {
    ObjectId::from_str(hex).map_err(|e| Error::Git(format!("invalid oid `{hex}`: {e}")))
}

fn log_message(action: &str, name: &str) -> gix::bstr::BString {
    format!("git-mesh: {action} {name}").into()
}

pub(crate) fn apply_ref_transaction_repo(
    repo: &gix::Repository,
    updates: &[RefUpdate],
) -> Result<()> {
    let mut edits: Vec<RefEdit> = Vec::with_capacity(updates.len());
    for update in updates {
        let edit = match update {
            RefUpdate::Create { name, new_oid } => RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: log_message("create", name),
                    },
                    expected: PreviousValue::MustNotExist,
                    new: Target::Object(parse_oid(new_oid)?),
                },
                name: name
                    .as_str()
                    .try_into()
                    .map_err(|e| Error::Git(format!("invalid ref name `{name}`: {e}")))?,
                deref: false,
            },
            RefUpdate::Update {
                name,
                new_oid,
                expected_old_oid,
            } => RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: log_message("update", name),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(parse_oid(
                        expected_old_oid,
                    )?)),
                    new: Target::Object(parse_oid(new_oid)?),
                },
                name: name
                    .as_str()
                    .try_into()
                    .map_err(|e| Error::Git(format!("invalid ref name `{name}`: {e}")))?,
                deref: false,
            },
            RefUpdate::Delete {
                name,
                expected_old_oid,
            } => RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch(Target::Object(parse_oid(
                        expected_old_oid,
                    )?)),
                    log: RefLog::AndReference,
                },
                name: name
                    .as_str()
                    .try_into()
                    .map_err(|e| Error::Git(format!("invalid ref name `{name}`: {e}")))?,
                deref: false,
            },
        };
        edits.push(edit);
    }
    repo.edit_references(edits)
        .map_err(|e| Error::Git(format!("ref transaction: {e}")))?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn is_reference_transaction_conflict(err: &Error) -> bool {
    let message = err.to_string();
    message.contains("cannot lock ref")
        || message.contains("reference already exists")
        || message.contains("is at ")
        || message.contains("expected ")
}

// ---------------------------------------------------------------------------
// Primitive gix helpers.
// ---------------------------------------------------------------------------

pub(crate) fn work_dir(repo: &gix::Repository) -> Result<&Path> {
    repo.workdir()
        .ok_or_else(|| Error::Git("bare repositories are not supported".into()))
}

/// Per-repository git directory. For a linked worktree this resolves
/// to `<main-git-dir>/worktrees/<id>` rather than `<workdir>/.git`,
/// which in a worktree is a pointer file (not a directory). All
/// `mesh/` filesystem state must be anchored here, not under the
/// workdir's `.git`.
pub(crate) fn git_dir(repo: &gix::Repository) -> &Path {
    repo.git_dir()
}

/// Per-repository `mesh/` root inside the git dir. Use this — never
/// `work_dir().join(".git").join("mesh")` — so worktrees and bare-ish
/// configurations resolve correctly.
pub(crate) fn mesh_dir(repo: &gix::Repository) -> std::path::PathBuf {
    git_dir(repo).join("mesh")
}

/// Common (shared) git directory. For a linked worktree this points at
/// the main repository's `.git/`, where shared state like `config` and
/// `lfs/objects/` lives.
pub(crate) fn common_dir(repo: &gix::Repository) -> &Path {
    repo.common_dir()
}

/// Write raw bytes as a blob and return its hex OID.
pub(crate) fn write_blob_bytes(repo: &gix::Repository, bytes: &[u8]) -> Result<String> {
    let id = repo
        .write_blob(bytes)
        .map_err(|e| Error::Git(format!("write blob: {e}")))?;
    Ok(id.detach().to_string())
}

/// List ref names with a given prefix (e.g. `refs/meshes/v1/`), returning
/// the basename component after the prefix.
pub(crate) fn list_refs_stripped(repo: &gix::Repository, prefix: &str) -> Result<Vec<String>> {
    let iter = repo
        .references()
        .map_err(|e| Error::Git(format!("refs: {e}")))?;
    let full = if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    };
    let platform = iter
        .prefixed(full.as_str())
        .map_err(|e| Error::Git(format!("refs prefix: {e}")))?;
    let mut out = Vec::new();
    for r in platform {
        let r = r.map_err(|e| Error::Git(format!("ref iter: {e}")))?;
        let full_name = r.name().as_bstr().to_string();
        if let Some(rest) = full_name.strip_prefix(&full) {
            out.push(rest.to_string());
        }
    }
    Ok(out)
}

/// List ref names and peeled target OIDs with a given prefix, returning the
/// basename component after the prefix plus the object id.
pub(crate) fn list_refs_stripped_with_oids(
    repo: &gix::Repository,
    prefix: &str,
) -> Result<Vec<(String, String)>> {
    let iter = repo
        .references()
        .map_err(|e| Error::Git(format!("refs: {e}")))?;
    let full = if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    };
    let platform = iter
        .prefixed(full.as_str())
        .map_err(|e| Error::Git(format!("refs prefix: {e}")))?;
    let mut out = Vec::new();
    for r in platform {
        let mut r = r.map_err(|e| Error::Git(format!("ref iter: {e}")))?;
        let full_name = r.name().as_bstr().to_string();
        if let Some(rest) = full_name.strip_prefix(&full) {
            let oid = r
                .peel_to_id()
                .map_err(|e| Error::Git(format!("peel ref `{full_name}`: {e}")))?
                .detach()
                .to_string();
            out.push((rest.to_string(), oid));
        }
    }
    Ok(out)
}

/// Resolve `HEAD` to a commit OID.
pub(crate) fn head_oid(repo: &gix::Repository) -> Result<String> {
    let id = repo
        .head_id()
        .map_err(|e| Error::Git(format!("resolve HEAD: {e}")))?;
    Ok(id.detach().to_string())
}

/// Return the tree OID of a commit.
pub(crate) fn commit_tree_oid(repo: &gix::Repository, commit_oid: &str) -> Result<String> {
    let oid = parse_oid(commit_oid)?;
    let commit = repo
        .find_commit(oid)
        .map_err(|e| Error::Git(format!("find commit `{commit_oid}`: {e}")))?;
    Ok(commit
        .tree_id()
        .map_err(|e| Error::Git(format!("commit tree: {e}")))?
        .detach()
        .to_string())
}

// ---------------------------------------------------------------------------
// git_log_name_only — history channel helper for the suggest detector.
// ---------------------------------------------------------------------------

/// One commit's hash and the set of paths changed by it (vs its first parent).
///
/// Mirrors the JS `{ hash, files }` shape in `loadGitHistory`.
#[derive(Clone, Debug)]
pub struct CommitChanges {
    pub hash: String,
    /// Paths changed in this commit relative to its first parent.
    /// For the root commit the parent is the empty tree.
    pub changed_paths: Vec<String>,
}

/// Walk HEAD's first `n` ancestors (no-merges) via `gix` and return each
/// commit's changed paths via tree-to-tree diff against its first parent.
///
/// Equivalent to `git log --name-only --no-merges -n N --pretty=format:commit:%H`
/// but implemented entirely via the `gix` library.
///
/// Results are in git-log order (most recent first). Merge commits are excluded.
pub fn git_log_name_only(repo: &gix::Repository, n: usize) -> Result<Vec<CommitChanges>> {
    let head_id = repo
        .head_id()
        .map_err(|e| Error::Git(format!("resolve HEAD: {e}")))?
        .detach();

    let walk = repo
        .rev_walk([head_id])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()
        .map_err(|e| Error::Git(format!("rev walk: {e}")))?;

    let mut out = Vec::with_capacity(n.min(512));

    for info in walk {
        if out.len() >= n {
            break;
        }
        let info = info.map_err(|e| Error::Git(format!("rev walk next: {e}")))?;
        let commit = repo
            .find_commit(info.id)
            .map_err(|e| Error::Git(format!("find commit {}: {e}", info.id)))?;

        // Skip merge commits (more than one parent) — matches `--no-merges`.
        let parent_ids: Vec<_> = commit.parent_ids().map(|p| p.detach()).collect();
        if parent_ids.len() > 1 {
            continue;
        }

        let new_tree = commit
            .tree()
            .map_err(|e| Error::Git(format!("commit tree {}: {e}", info.id)))?;

        let old_tree = match parent_ids.first() {
            Some(pid) => match repo.find_commit(*pid) {
                Ok(parent) => parent.tree().unwrap_or_else(|_| repo.empty_tree()),
                Err(_) => repo.empty_tree(),
            },
            None => repo.empty_tree(),
        };

        // Disable rename tracking — we only want which paths changed,
        // not rename pairing (matches `git log --name-only` defaults).
        let mut opts = gix::diff::Options::default();
        opts.track_rewrites(None);

        let changes = repo
            .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(opts))
            .map_err(|e| Error::Git(format!("diff tree {}: {e}", info.id)))?;

        let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for change in &changes {
            use gix::object::tree::diff::ChangeDetached;
            match change {
                ChangeDetached::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | ChangeDetached::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    // Only record blob entries; skip tree/directory entries.
                    if !entry_mode.is_blob_or_symlink() {
                        continue;
                    }
                    paths.insert(
                        std::str::from_utf8(location.as_slice())
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
                ChangeDetached::Modification {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_blob_or_symlink() {
                        continue;
                    }
                    paths.insert(
                        std::str::from_utf8(location.as_slice())
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
                ChangeDetached::Rewrite {
                    source_location,
                    source_entry_mode,
                    location,
                    entry_mode,
                    ..
                } => {
                    if source_entry_mode.is_blob_or_symlink() {
                        paths.insert(
                            std::str::from_utf8(source_location.as_slice())
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                    if entry_mode.is_blob_or_symlink() {
                        paths.insert(
                            std::str::from_utf8(location.as_slice())
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
            }
        }

        out.push(CommitChanges {
            hash: info.id.to_string(),
            changed_paths: paths.into_iter().collect(),
        });
    }

    Ok(out)
}

/// Extracted commit metadata.
#[derive(Clone, Debug)]
pub(crate) struct CommitMeta {
    pub author_name: String,
    pub author_email: String,
    pub author_date_rfc2822: String,
    pub committer_time: i64,
    pub summary: String,
    pub message: String,
}

pub(crate) fn commit_meta(repo: &gix::Repository, commit_oid: &str) -> Result<CommitMeta> {
    let oid = parse_oid(commit_oid)?;
    let commit = repo
        .find_commit(oid)
        .map_err(|e| Error::Git(format!("find commit `{commit_oid}`: {e}")))?;
    let decoded = commit
        .decode()
        .map_err(|e| Error::Git(format!("decode commit: {e}")))?;
    let author_sig = decoded
        .author()
        .map_err(|e| Error::Git(format!("author: {e}")))?;
    let committer_sig = decoded
        .committer()
        .map_err(|e| Error::Git(format!("committer: {e}")))?;
    let author_time = author_sig
        .time()
        .map_err(|e| Error::Git(format!("author time: {e}")))?;
    let committer_time = committer_sig
        .time()
        .map_err(|e| Error::Git(format!("committer time: {e}")))?;
    let message = decoded.message.to_string();
    let summary = message.lines().next().unwrap_or("").to_string();
    Ok(CommitMeta {
        author_name: author_sig.name.to_string(),
        author_email: author_sig.email.to_string(),
        author_date_rfc2822: format_rfc2822(author_time),
        committer_time: committer_time.seconds,
        summary,
        message,
    })
}

fn format_rfc2822(t: gix::date::Time) -> String {
    // Produce `Thu, 1 Jan 1970 00:00:00 +0000` style matching `git show --format=%aD`.
    use chrono::{DateTime, FixedOffset};
    let secs = t.seconds;
    let offset_secs = t.offset;
    let fixed =
        FixedOffset::east_opt(offset_secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let dt_utc = DateTime::from_timestamp(secs, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
    let dt: DateTime<FixedOffset> = dt_utc.with_timezone(&fixed);
    dt.format("%a, %-d %b %Y %H:%M:%S %z").to_string()
}

/// Walk commits reachable from `head` but not from any of `excludes`,
/// returning hex OIDs in topological order (newest first), optionally capped.
pub(crate) fn rev_walk_excluding(
    repo: &gix::Repository,
    heads: &[&str],
    excludes: &[&str],
    limit: Option<usize>,
) -> Result<Vec<String>> {
    let head_ids: Vec<ObjectId> = heads.iter().map(|h| parse_oid(h)).collect::<Result<_>>()?;
    let exclude_ids: Vec<ObjectId> = excludes
        .iter()
        .map(|h| parse_oid(h))
        .collect::<Result<_>>()?;
    let mut walk = repo
        .rev_walk(head_ids)
        .with_hidden(exclude_ids)
        .all()
        .map_err(|e| Error::Git(format!("rev walk: {e}")))?;
    let mut out = Vec::new();
    for info in walk.by_ref() {
        let info = info.map_err(|e| Error::Git(format!("rev walk next: {e}")))?;
        out.push(info.id.to_string());
        if let Some(n) = limit
            && out.len() >= n
        {
            break;
        }
    }
    Ok(out)
}

/// Is `anchor` reachable from any reference in the repository?
// Unused while the resolver is stubbed in the Phase 1 types slice; the
// engine slice re-wires it through the new `resolver::Engine`.
#[allow(dead_code)]
pub(crate) fn commit_reachable_from_any_ref(repo: &gix::Repository, anchor: &str) -> Result<bool> {
    let anchor_id = match parse_oid(anchor) {
        Ok(id) => id,
        Err(_) => return Ok(false),
    };
    if let Ok(head_id) = repo.head_id().map(|id| id.detach()) {
        if head_id == anchor_id {
            return Ok(true);
        }
        if let Ok(base) = repo.merge_base(head_id, anchor_id)
            && base.detach() == anchor_id
        {
            return Ok(true);
        }
    }
    let refs = repo
        .references()
        .map_err(|e| Error::Git(format!("refs: {e}")))?;
    let all = refs
        .all()
        .map_err(|e| Error::Git(format!("refs all: {e}")))?;
    for r in all {
        let mut r = match r {
            Ok(r) => r,
            Err(_) => continue,
        };
        let tip = match r.peel_to_id() {
            Ok(id) => id.detach(),
            Err(_) => continue,
        };
        if tip == anchor_id {
            return Ok(true);
        }
        // merge_base(tip, anchor) == anchor → anchor is ancestor of tip
        match repo.merge_base(tip, anchor_id) {
            Ok(base) if base.detach() == anchor_id => return Ok(true),
            _ => continue,
        }
    }
    Ok(false)
}

/// Create a commit object (without updating any ref) and return its hex OID.
///
/// Uses the repository's configured author/committer; callers/tests that need
/// a fixed identity should set `GIT_AUTHOR_*` / `GIT_COMMITTER_*` env vars,
/// which gix honors.
pub fn create_commit(
    repo: &gix::Repository,
    tree_oid: &str,
    message: &str,
    parents: &[String],
) -> Result<String> {
    let tree = parse_oid(tree_oid)?;
    let parent_ids: Vec<ObjectId> = parents
        .iter()
        .map(|p| parse_oid(p))
        .collect::<Result<_>>()?;
    let commit = repo
        .new_commit(message, tree, parent_ids)
        .map_err(|e| Error::Git(format!("create commit: {e}")))?;
    Ok(commit.id.to_string())
}

pub(crate) fn resolve_ref_oid_optional(work_dir: &Path, ref_name: &str) -> Result<Option<String>> {
    let repo = gix::open(work_dir).map_err(|e| Error::Git(format!("open repo: {e}")))?;
    resolve_ref_oid_optional_repo(&repo, ref_name)
}

pub(crate) fn resolve_ref_oid_optional_repo(
    repo: &gix::Repository,
    ref_name: &str,
) -> Result<Option<String>> {
    match repo
        .try_find_reference(ref_name)
        .map_err(|e| Error::Git(format!("find ref `{ref_name}`: {e}")))?
    {
        Some(mut r) => {
            let id = r
                .peel_to_id()
                .map_err(|e| Error::Git(format!("peel ref `{ref_name}`: {e}")))?;
            Ok(Some(id.detach().to_string()))
        }
        None => Ok(None),
    }
}

pub(crate) fn read_blob_bytes(repo: &gix::Repository, blob_oid: &str) -> Result<Vec<u8>> {
    blob_data(repo, blob_oid)
}

fn blob_data(repo: &gix::Repository, blob_oid: &str) -> Result<Vec<u8>> {
    let oid = parse_oid(blob_oid)?;
    let obj = repo
        .find_object(oid)
        .map_err(|e| Error::Git(format!("find object `{blob_oid}`: {e}")))?;
    Ok(obj.into_blob().detach().data)
}

// ---------------------------------------------------------------------------
// Typed public helpers (Slice B signatures).
// ---------------------------------------------------------------------------

/// Read a blob object as UTF-8 text (anchor records, config blobs, etc).
pub fn read_git_text(repo: &gix::Repository, oid: &str) -> Result<String> {
    let data = blob_data(repo, oid)?;
    String::from_utf8(data).map_err(|e| Error::Parse(format!("object not utf-8: {e}")))
}

/// Resolve a commit-ish to a full commit OID.
///
/// Errors are curated at this boundary so the upstream `gix-revision`
/// `Display` (which embeds `/.cargo/registry/.../gix-revision-x.y.z/src/...rs:NNN`)
/// never reaches CLI stderr. Callers prefix the originating flag (e.g.
/// `--since`, `--at`) themselves.
pub fn resolve_commit(repo: &gix::Repository, commit_ish: &str) -> Result<String> {
    let id = repo
        .rev_parse_single(commit_ish)
        .map_err(|e| Error::Git(curate_rev_parse_error(commit_ish, &e.to_string())))?;
    Ok(id.detach().to_string())
}

/// Translate the upstream `gix-revision` error string into a clean,
/// host-stable message. Recognized variants:
///
/// - `couldn't parse revision` → "not a valid revision"
/// - `delegate.traverse(NthAncestor(N))` → "has fewer than N ancestors"
/// - anything else → "could not resolve `<rev>`"
fn curate_rev_parse_error(commit_ish: &str, raw: &str) -> String {
    if let Some(after) = raw.split("NthAncestor(").nth(1)
        && let Some(num_str) = after.split(')').next()
        && let Ok(n) = num_str.parse::<u64>()
    {
        return format!("has fewer than {n} ancestors");
    }
    if raw.contains("couldn't parse revision") {
        return "not a valid revision".to_string();
    }
    format!("could not resolve `{commit_ish}`")
}

/// True if `ancestor` is an ancestor of `descendant` (or equal).
pub fn is_ancestor(repo: &gix::Repository, ancestor: &str, descendant: &str) -> Result<bool> {
    let ancestor_id = repo
        .rev_parse_single(ancestor)
        .map_err(|e| Error::Git(format!("rev-parse `{ancestor}`: {e}")))?
        .detach();
    let descendant_id = repo
        .rev_parse_single(descendant)
        .map_err(|e| Error::Git(format!("rev-parse `{descendant}`: {e}")))?
        .detach();
    if ancestor_id == descendant_id {
        return Ok(true);
    }
    match repo.merge_base(ancestor_id, descendant_id) {
        Ok(base) => Ok(base.detach() == ancestor_id),
        Err(_) => Ok(false),
    }
}

/// Read the blob OID of `path` at `commit_oid`'s tree.
pub fn path_blob_at(repo: &gix::Repository, commit_oid: &str, path: &str) -> Result<String> {
    let oid = parse_oid(commit_oid).map_err(|_| Error::PathNotInTree {
        path: path.to_string(),
        commit: commit_oid.to_string(),
    })?;
    let commit = repo.find_commit(oid).map_err(|_| Error::PathNotInTree {
        path: path.to_string(),
        commit: commit_oid.to_string(),
    })?;
    let mut tree = commit.tree().map_err(|_| Error::PathNotInTree {
        path: path.to_string(),
        commit: commit_oid.to_string(),
    })?;
    let entry = tree
        .peel_to_entry_by_path(Path::new(path))
        .map_err(|_| Error::PathNotInTree {
            path: path.to_string(),
            commit: commit_oid.to_string(),
        })?
        .ok_or_else(|| Error::PathNotInTree {
            path: path.to_string(),
            commit: commit_oid.to_string(),
        })?;
    Ok(entry.object_id().to_string())
}

/// Read file bytes from the working tree, relative to the repo root.
pub fn read_worktree_bytes(repo: &gix::Repository, path: &str) -> Result<Vec<u8>> {
    let wd = work_dir(repo)?;
    Ok(std::fs::read(wd.join(path))?)
}

/// Line count of `blob_oid`.
pub fn blob_line_count(repo: &gix::Repository, blob_oid: &str) -> Result<u32> {
    let data = blob_data(repo, blob_oid)?;
    let text =
        std::str::from_utf8(&data).map_err(|e| Error::Parse(format!("blob not utf-8: {e}")))?;
    Ok(text.lines().count() as u32)
}

/// Extract lines `[start, end]` (1-based inclusive) from a blob.
pub fn extract_blob_lines(
    repo: &gix::Repository,
    blob_oid: &str,
    start: u32,
    end: u32,
) -> Result<Vec<u8>> {
    let data = blob_data(repo, blob_oid)?;
    let text =
        std::str::from_utf8(&data).map_err(|e| Error::Parse(format!("blob not utf-8: {e}")))?;
    let lines: Vec<&str> = text.lines().collect();
    let lo = start.saturating_sub(1) as usize;
    let hi = (end as usize).min(lines.len());
    if lo > hi {
        return Err(Error::InvalidAnchor { start, end });
    }
    let mut out = String::new();
    for line in &lines[lo..hi] {
        out.push_str(line);
        out.push('\n');
    }
    Ok(out.into_bytes())
}

/// Placeholder for §5.1 per-commit `log -L` walker. Implemented inside
/// [`crate::resolver`] for now; kept here as an unimplemented hook.
pub fn log_l_resolve(
    _repo: &gix::Repository,
    _anchor_sha: &str,
    _path: &str,
    _start: u32,
    _end: u32,
    _copy_detection: crate::types::CopyDetection,
) -> Result<Option<(String, u32, u32, String)>> {
    // Resolver lives in stale.rs (ported from v1). This hook exists only
    // to preserve the Slice B signature.
    Err(Error::Git(
        "git::log_l_resolve is not used; call resolver::resolve_anchor".into(),
    ))
}

/// Placeholder for a standalone culprit helper; the resolver drives its
/// own blame walk in [`crate::resolver::culprit_commit`].
pub fn culprit_commit(
    _repo: &gix::Repository,
    _anchor_sha: &str,
    _path: &str,
    _start: u32,
    _end: u32,
) -> Result<Option<String>> {
    Err(Error::Git(
        "git::culprit_commit is not used; call resolver::culprit_commit".into(),
    ))
}

// ---------------------------------------------------------------------------
// Slice 1: shared gix helpers (replacements for `Command::new("git")`).
// ---------------------------------------------------------------------------

/// Resolve `commit_ish`, peel to its tree, and look up `path` within it.
///
/// Returns `Ok(None)` when `path` isn't present in the tree (matches the
/// "no row" semantics of `git ls-tree <sha> -- <path>` we are replacing).
/// Returns `Err` only for plumbing failures (bad commit-ish, unreadable
/// objects, ill-formed UTF-8 path components).
pub fn tree_entry_at(
    repo: &gix::Repository,
    commit_ish: &str,
    path: &Path,
) -> Result<Option<(gix::objs::tree::EntryMode, ObjectId)>> {
    let id = match repo.rev_parse_single(commit_ish) {
        Ok(id) => id,
        Err(_) => return Ok(None),
    };
    let object = id
        .object()
        .map_err(|e| Error::Git(format!("find object `{commit_ish}`: {e}")))?;
    let tree = object
        .peel_to_tree()
        .map_err(|e| Error::Git(format!("peel `{commit_ish}` to tree: {e}")))?;
    let entry = tree
        .lookup_entry_by_path(path)
        .map_err(|e| Error::Git(format!("lookup entry `{}`: {e}", path.display())))?;
    Ok(entry.map(|e| (e.mode(), e.object_id())))
}

/// Snapshot of an index entry used by callers that previously parsed
/// `git ls-files --stage` / `git ls-files -u -z` lines.
#[derive(Clone, Debug)]
pub struct IndexEntrySnapshot {
    pub mode: gix::objs::tree::EntryMode,
    pub oid: ObjectId,
    pub stage: gix::index::entry::Stage,
    pub path: String,
}

/// Load the worktree index (or synthesize it from `HEAD^{tree}` if there
/// is no on-disk index yet) and return one snapshot per entry.
///
/// Returning owned snapshots keeps the borrow shape simple at call sites
/// that want to filter / collect without keeping the index file alive.
pub fn index_entries(repo: &gix::Repository) -> Result<Vec<IndexEntrySnapshot>> {
    let idx = repo
        .index_or_load_from_head()
        .map_err(|e| Error::Git(format!("load index: {e}")))?;
    let state = &*idx;
    let mut out = Vec::with_capacity(state.entries().len());
    for entry in state.entries() {
        let mode = match gix::objs::tree::EntryMode::try_from(entry.mode.bits()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let path = entry.path(state).to_string();
        out.push(IndexEntrySnapshot {
            mode,
            oid: entry.id,
            stage: entry.stage(),
            path,
        });
    }
    Ok(out)
}

/// Compute the SHA-1 a blob with `bytes` would have, without writing it
/// (replaces `git hash-object [--stdin] <…>`).
pub fn hash_blob(bytes: &[u8]) -> Result<ObjectId> {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, bytes)
        .map_err(|e| Error::Git(format!("hash-object: {e}")))
}

/// Resolve a single `.gitattributes` attribute for `rel_path` relative to
/// the repo's worktree root. Returns:
///
/// * `Ok(None)` when the attribute is unset / unspecified.
/// * `Ok(Some("set"))` for boolean-set attributes (`<attr>` or `<attr>=true`).
/// * `Ok(Some("<value>"))` for valued attributes.
///
/// The `binary` macro is expanded by `gix_attributes` automatically;
/// callers that need the macro itself should query `"binary"` directly.
pub fn attr_for(
    repo: &gix::Repository,
    rel_path: &Path,
    name: &str,
) -> Result<Option<gix::bstr::BString>> {
    let index = repo
        .index_or_load_from_head()
        .map_err(|e| Error::Git(format!("load index: {e}")))?;
    let mut stack = repo
        .attributes(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            None,
        )
        .map_err(|e| Error::Git(format!("attribute stack: {e}")))?;
    let mut outcome = stack.selected_attribute_matches([name]);
    let platform = stack
        .at_entry(rel_path, None)
        .map_err(|e| Error::Git(format!("attr at_entry `{}`: {e}", rel_path.display())))?;
    if !platform.matching_attributes(&mut outcome) {
        return Ok(None);
    }
    if let Some(m) = outcome.iter_selected().next() {
        return Ok(match m.assignment.state {
            gix::attrs::StateRef::Set => Some("set".into()),
            gix::attrs::StateRef::Unset => None,
            gix::attrs::StateRef::Value(v) => Some(v.as_bstr().to_owned()),
            gix::attrs::StateRef::Unspecified => None,
        });
    }
    Ok(None)
}

/// Slice 6d: read the effective value of `core.logAllRefUpdates`.
///
/// Returns `Some("always" | "true" | "false")` when set, or `None` when
/// unset. Custom refs outside `refs/heads`, `refs/remotes`, `refs/notes`,
/// and `HEAD` only get a reflog when this is set to `always`.
pub fn log_all_ref_updates_value(repo: &gix::Repository) -> Option<String> {
    repo.config_snapshot()
        .string("core.logAllRefUpdates")
        .map(|v| v.to_string())
}

/// Slice 6d: ensure `core.logAllRefUpdates` is set to a value that
/// covers refs outside the standard set (i.e. `always`). Idempotent —
/// if the value already covers `refs/meshes/*` (`always`) we leave it
/// alone.
///
/// Greenfield: we do not migrate `true` → `always` automatically; the
/// resolver's reflog story for mesh refs requires `always`, full stop.
pub fn ensure_log_all_ref_updates_always(repo: &gix::Repository) -> Result<()> {
    if log_all_ref_updates_value(repo).as_deref() == Some("always") {
        return Ok(());
    }
    write_local_config_value(repo, "core", None, "logAllRefUpdates", "always")
}

/// Slice 6c/6d helper: write a single key to the local `.git/config`,
/// replacing any existing value(s) for that key. Used for scalar config
/// values; multi-valued lists like `remote.<name>.fetch` need their own
/// helper.
pub(crate) fn write_local_config_value(
    repo: &gix::Repository,
    section: &'static str,
    subsection: Option<&str>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    let path = common_dir(repo).join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| Error::Git(format!("load config: {e}")))?;
    let sub_bstr = subsection.map(|s| s.as_bytes().into());
    let mut section_mut = file
        .section_mut_or_create_new(section, sub_bstr)
        .map_err(|e| Error::Git(format!("section: {e}")))?;
    section_mut.set(
        key.try_into()
            .map_err(|e| Error::Git(format!("key `{key}`: {e}")))?,
        value.as_bytes().into(),
    );
    let bytes = file.to_bstring();
    std::fs::write(&path, bytes.as_slice())
        .map_err(|e| Error::Git(format!("write config: {e}")))?;
    Ok(())
}

/// Read a single config string by full key (e.g. `"filter.lfs.process"`).
pub fn config_string(repo: &gix::Repository, key: &str) -> Option<String> {
    repo.config_snapshot().string(key).map(|v| v.to_string())
}

pub fn update_ref_cas(
    repo: &gix::Repository,
    ref_name: &str,
    new_oid: &str,
    expected_oid: Option<&str>,
) -> Result<()> {
    let wd = work_dir(repo)?;
    let updates = [match expected_oid {
        Some(prev) => RefUpdate::Update {
            name: ref_name.to_string(),
            new_oid: new_oid.to_string(),
            expected_old_oid: prev.to_string(),
        },
        None => RefUpdate::Create {
            name: ref_name.to_string(),
            new_oid: new_oid.to_string(),
        },
    }];
    apply_ref_transaction(wd, &updates)
}

pub fn delete_ref(repo: &gix::Repository, ref_name: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    let current = resolve_ref_oid_optional(wd, ref_name)?
        .ok_or_else(|| Error::Git(format!("ref not found: {ref_name}")))?;
    apply_ref_transaction(
        wd,
        &[RefUpdate::Delete {
            name: ref_name.to_string(),
            expected_old_oid: current,
        }],
    )
}

#[cfg(test)]
mod gix_helper_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn seed_repo() -> (tempfile::TempDir, gix::Repository, String) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(
            dir.join(".gitattributes"),
            "*.bin binary\n*.tx filter=foo\n",
        )
        .unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let head = String::from_utf8(
            Command::new("git")
                .current_dir(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let repo = gix::open(dir).unwrap();
        (td, repo, head)
    }

    #[test]
    fn tree_entry_at_finds_blob() {
        let (_td, repo, head) = seed_repo();
        let entry = tree_entry_at(&repo, &head, Path::new("a.txt")).unwrap();
        let (_mode, oid) = entry.expect("a.txt should exist at HEAD");
        assert_eq!(oid.to_string().len(), 40);
    }

    #[test]
    fn tree_entry_at_missing_returns_none() {
        let (_td, repo, head) = seed_repo();
        let out = tree_entry_at(&repo, &head, Path::new("nope.txt")).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn index_entries_returns_committed_files() {
        let (_td, repo, _head) = seed_repo();
        let entries = index_entries(&repo).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&".gitattributes"));
        assert!(
            entries
                .iter()
                .all(|e| e.stage == gix::index::entry::Stage::Unconflicted)
        );
    }

    #[test]
    fn hash_blob_matches_git() {
        let (td, _repo, _head) = seed_repo();
        let bytes = b"hello world\n";
        let oid = hash_blob(bytes).unwrap();
        use std::io::Write;
        let mut child = Command::new("git")
            .current_dir(td.path())
            .args(["hash-object", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
        let res = child.wait_with_output().unwrap();
        let expected = String::from_utf8(res.stdout).unwrap().trim().to_string();
        assert_eq!(oid.to_string(), expected);
    }

    #[test]
    fn attr_for_reads_filter_and_binary() {
        let (_td, repo, _head) = seed_repo();
        std::fs::write(_td.path().join("x.tx"), "y").unwrap();
        std::fs::write(_td.path().join("y.bin"), [0u8, 1u8]).unwrap();
        let f = attr_for(&repo, Path::new("x.tx"), "filter").unwrap();
        assert_eq!(f.as_ref().map(|b| b.to_string()), Some("foo".to_string()));
        let b = attr_for(&repo, Path::new("y.bin"), "binary").unwrap();
        // `binary` is a macro; resolves as Set when it matches.
        assert_eq!(b.as_ref().map(|s| s.to_string()), Some("set".to_string()));
        let none = attr_for(&repo, Path::new("a.txt"), "filter").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn config_string_reads_value() {
        let (td, repo, _head) = seed_repo();
        run_git(
            td.path(),
            &["config", "filter.lfs.process", "git-lfs filter-process"],
        );
        let repo = gix::open(repo.path()).unwrap();
        assert_eq!(
            config_string(&repo, "filter.lfs.process").as_deref(),
            Some("git-lfs filter-process"),
        );
        assert!(config_string(&repo, "no.such.key").is_none());
    }
}
