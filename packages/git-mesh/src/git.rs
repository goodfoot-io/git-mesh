//! Git plumbing helpers.
//!
//! Thin wrappers around the `git` subprocess (and `gix` where applicable).
//! These are the only place in the crate that talks to git directly; the
//! rest of the crate stays on typed results via [`crate::Result`].

use crate::{Error, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
    let mut input = String::from("start\n");
    for update in updates {
        match update {
            RefUpdate::Create { name, new_oid } => {
                input.push_str(&format!("create {name} {new_oid}\n"));
            }
            RefUpdate::Update {
                name,
                new_oid,
                expected_old_oid,
            } => {
                input.push_str(&format!("update {name} {new_oid} {expected_old_oid}\n"));
            }
            RefUpdate::Delete {
                name,
                expected_old_oid,
            } => {
                input.push_str(&format!("delete {name} {expected_old_oid}\n"));
            }
        }
    }
    input.push_str("prepare\ncommit\n");
    git_with_input(work_dir, ["update-ref", "--stdin"], &input)?;
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
    String::from_utf8(output.stdout)
        .map_err(|e| Error::Parse(format!("git output not utf-8: {e}")))
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

pub(crate) fn git_stdout_with_identity<I, S>(work_dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let output = Command::new("git")
        .current_dir(work_dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
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
    git_stdout_optional(
        work_dir,
        ["rev-parse", "--verify", "--quiet", ref_name],
    )
}

pub(crate) fn git_show_file_lines(
    work_dir: &Path,
    commit_oid: &str,
    path: &str,
) -> Result<Vec<String>> {
    let output = git_stdout(work_dir, ["show", &format!("{commit_oid}:{path}")])?;
    Ok(output
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

// ---------------------------------------------------------------------------
// Typed public helpers (Slice B signatures).
// ---------------------------------------------------------------------------

/// Read a git object as UTF-8 text (blob contents, commit messages, etc).
pub fn read_git_text(repo: &gix::Repository, oid: &str) -> Result<String> {
    let wd = work_dir(repo)?;
    git_stdout(wd, ["cat-file", "-p", oid])
}

/// Resolve a commit-ish to a full commit OID.
pub fn resolve_commit(repo: &gix::Repository, commit_ish: &str) -> Result<String> {
    let wd = work_dir(repo)?;
    git_stdout(wd, ["rev-parse", commit_ish])
}

/// True if `ancestor` is an ancestor of `descendant` (or equal).
pub fn is_ancestor(repo: &gix::Repository, ancestor: &str, descendant: &str) -> Result<bool> {
    let wd = work_dir(repo)?;
    let status = Command::new("git")
        .current_dir(wd)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Git("merge-base --is-ancestor failed".into())),
    }
}

/// Read the blob OID of `path` at `commit_oid`'s tree.
pub fn path_blob_at(repo: &gix::Repository, commit_oid: &str, path: &str) -> Result<String> {
    let wd = work_dir(repo)?;
    match git_stdout(wd, ["rev-parse", &format!("{commit_oid}:{path}")]) {
        Ok(oid) => Ok(oid),
        Err(_) => Err(Error::PathNotInTree {
            path: path.to_string(),
            commit: commit_oid.to_string(),
        }),
    }
}

/// Read file bytes from the working tree, relative to the repo root.
pub fn read_worktree_bytes(repo: &gix::Repository, path: &str) -> Result<Vec<u8>> {
    let wd = work_dir(repo)?;
    Ok(std::fs::read(wd.join(path))?)
}

/// Line count of `blob_oid`.
pub fn blob_line_count(repo: &gix::Repository, blob_oid: &str) -> Result<u32> {
    let wd = work_dir(repo)?;
    let contents = git_stdout_raw(wd, ["cat-file", "-p", blob_oid])?;
    Ok(contents.lines().count() as u32)
}

/// Extract lines `[start, end]` (1-based inclusive) from a blob.
pub fn extract_blob_lines(
    repo: &gix::Repository,
    blob_oid: &str,
    start: u32,
    end: u32,
) -> Result<Vec<u8>> {
    let wd = work_dir(repo)?;
    let contents = git_stdout_raw(wd, ["cat-file", "-p", blob_oid])?;
    let lines: Vec<&str> = contents.lines().collect();
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
