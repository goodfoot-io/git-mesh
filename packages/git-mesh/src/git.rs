//! Git plumbing helpers.
//!
//! Thin wrappers around the `git` subprocess (and `gix` where applicable).
//! These are the only place in the crate that talks to git directly; the
//! rest of the crate stays on typed results via [`crate::Result`].

use crate::{Error, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
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
// Primitive git subprocess helpers (ported).
// ---------------------------------------------------------------------------

pub(crate) fn work_dir(repo: &gix::Repository) -> Result<&Path> {
    repo.workdir()
        .ok_or_else(|| Error::Git("bare repositories are not supported".into()))
}

pub(crate) fn git_stdout<I, S>(work_dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let output = Command::new("git")
        .current_dir(work_dir)
        .args(args.into_iter().map(|arg| arg.as_ref().to_string()))
        .output()?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| Error::Parse(format!("git output not utf-8: {e}")))
}

pub(crate) fn git_stdout_raw<I, S>(work_dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let output = Command::new("git")
        .current_dir(work_dir)
        .args(args.into_iter().map(|arg| arg.as_ref().to_string()))
        .output()?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| Error::Parse(format!("git output not utf-8: {e}")))
}

pub(crate) fn git_stdout_optional<I, S>(work_dir: &Path, args: I) -> Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let output = Command::new("git")
        .current_dir(work_dir)
        .args(args.into_iter().map(|arg| arg.as_ref().to_string()))
        .output()?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(output.stdout)
                .map_err(|e| Error::Parse(format!("git output not utf-8: {e}")))?
                .trim()
                .to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )),
    }
}

pub(crate) fn git_stdout_lines<I, S>(work_dir: &Path, args: I) -> Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    Ok(git_stdout_optional(work_dir, args)?
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
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

pub(crate) fn git_with_input<I, S>(work_dir: &Path, args: I, input: &str) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut child = Command::new("git")
        .current_dir(work_dir)
        .args(args.into_iter().map(|arg| arg.as_ref().to_string()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Git("missing stdin on child".into()))?;
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| Error::Parse(format!("git output not utf-8: {e}")))
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

pub(crate) fn git_show_file_lines(
    work_dir: &Path,
    commit_oid: &str,
    path: &str,
) -> Result<Vec<String>> {
    let repo = gix::open(work_dir).map_err(|e| Error::Git(format!("open repo: {e}")))?;
    let blob_oid = path_blob_at(&repo, commit_oid, path)?;
    let data = blob_data(&repo, &blob_oid)?;
    let text =
        std::str::from_utf8(&data).map_err(|e| Error::Parse(format!("blob not utf-8: {e}")))?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
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

/// Read a blob object as UTF-8 text (range records, config blobs, etc).
pub fn read_git_text(repo: &gix::Repository, oid: &str) -> Result<String> {
    let data = blob_data(repo, oid)?;
    String::from_utf8(data).map_err(|e| Error::Parse(format!("object not utf-8: {e}")))
}

/// Resolve a commit-ish to a full commit OID.
pub fn resolve_commit(repo: &gix::Repository, commit_ish: &str) -> Result<String> {
    let id = repo
        .rev_parse_single(commit_ish)
        .map_err(|e| Error::Git(format!("rev-parse `{commit_ish}`: {e}")))?;
    Ok(id.detach().to_string())
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
        return Err(Error::InvalidRange { start, end });
    }
    let mut out = String::new();
    for line in &lines[lo..hi] {
        out.push_str(line);
        out.push('\n');
    }
    Ok(out.into_bytes())
}

/// Placeholder for §5.1 per-commit `log -L` walker. Implemented inside
/// [`crate::stale`] for now; kept here as an unimplemented hook.
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
        "git::log_l_resolve is not used; call stale::resolve_range".into(),
    ))
}

/// Placeholder for a standalone culprit helper; the resolver drives its
/// own blame walk in [`crate::stale::culprit_commit`].
pub fn culprit_commit(
    _repo: &gix::Repository,
    _anchor_sha: &str,
    _path: &str,
    _start: u32,
    _end: u32,
) -> Result<Option<String>> {
    Err(Error::Git(
        "git::culprit_commit is not used; call stale::culprit_commit".into(),
    ))
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
