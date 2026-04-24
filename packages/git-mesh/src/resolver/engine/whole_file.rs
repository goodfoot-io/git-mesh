//! Whole-file resolver: blob-OID equality at the deepest enabled layer
//! per plan §D2. Renames produce `Moved`; symlinks/gitlinks compare by
//! recorded blob/SHA.

use super::EngineState;
use crate::git;
use crate::types::{
    DriftSource, MeshConfig, Range, RangeExtent, RangeLocation, RangeResolved, RangeStatus,
};
use crate::{Error, Result};
use std::path::PathBuf;
use std::str::FromStr;

fn oid_from_hex(hex: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_str(hex).map_err(|e| Error::Git(format!("invalid oid `{hex}`: {e}")))
}

fn is_commit_reachable(repo: &gix::Repository, commit: &str) -> Result<bool> {
    git::commit_reachable_from_any_ref(repo, commit)
}

pub(crate) fn resolve_whole_file(
    repo: &gix::Repository,
    state: &mut EngineState,
    cfg: &MeshConfig,
    range_id: &str,
    r: Range,
) -> Result<RangeResolved> {
    let anchored = RangeLocation {
        path: PathBuf::from(&r.path),
        extent: RangeExtent::Whole,
        blob: oid_from_hex(&r.blob).ok(),
    };
    if !is_commit_reachable(repo, &r.anchor_sha)? {
        return Ok(RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: RangeStatus::Orphaned,
            source: None,
            acknowledged_by: None,
            culprit: None,
        });
    }

    let head_sha = git::head_oid(repo)?;
    let workdir = git::work_dir(repo)?;
    let current_path = follow_path_to_head(workdir, &r.anchor_sha, &head_sha, &r.path)
        .unwrap_or_else(|| r.path.clone());

    let head_kind_sha = ls_tree_kind_and_sha(workdir, &head_sha, &current_path);
    let mut deepest = DriftSource::Head;
    let mut current_blob: Option<String> = head_kind_sha.as_ref().map(|(_, sha)| sha.clone());
    let moved = current_path != r.path;

    if state.layers.index {
        if let Some((_mode, sha)) = ls_files_stage(workdir, &current_path) {
            current_blob = Some(sha);
        }
        deepest = DriftSource::Index;
    }
    if state.layers.worktree {
        let abs = workdir.join(&current_path);
        if let Ok(md) = std::fs::symlink_metadata(&abs) {
            if md.file_type().is_symlink() {
                let target = std::fs::read_link(&abs)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let oid = git_hash_object_bytes(workdir, target.as_bytes());
                current_blob = oid;
            } else if md.file_type().is_file()
                && let Ok(oid) = git_hash_object_path(workdir, &abs)
            {
                current_blob = Some(oid);
            }
        } else {
            current_blob = None;
        }
        deepest = DriftSource::Worktree;
    }

    let _ = cfg;
    let status: RangeStatus;
    let source: Option<DriftSource>;
    let cur_blob_oid = current_blob.as_deref().and_then(|s| oid_from_hex(s).ok());
    let current_loc = Some(RangeLocation {
        path: PathBuf::from(&current_path),
        extent: RangeExtent::Whole,
        blob: cur_blob_oid,
    });
    match current_blob.as_deref() {
        None => {
            status = RangeStatus::Changed;
            source = Some(deepest);
        }
        Some(cur) if cur == r.blob && moved => {
            status = RangeStatus::Moved;
            source = Some(deepest);
        }
        Some(cur) if cur == r.blob => {
            status = RangeStatus::Fresh;
            source = None;
        }
        Some(_) => {
            status = RangeStatus::Changed;
            source = Some(deepest);
        }
    }

    Ok(RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current: current_loc,
        status,
        source,
        acknowledged_by: None,
        culprit: None,
    })
}

fn follow_path_to_head(
    workdir: &std::path::Path,
    anchor: &str,
    head: &str,
    path: &str,
) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args([
            "log", "--follow", "--name-only", "-z", "--format=",
            &format!("{anchor}..{head}"), "--", path,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    out.stdout
        .split(|b| *b == 0)
        .find(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
}

fn ls_tree_kind_and_sha(
    workdir: &std::path::Path,
    commit: &str,
    path: &str,
) -> Option<(String, String)> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-tree", commit, "--", path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?;
    let (meta, _) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let mode = parts.next()?.to_string();
    let _ty = parts.next()?;
    let oid = parts.next()?.to_string();
    Some((mode, oid))
}

fn ls_files_stage(workdir: &std::path::Path, path: &str) -> Option<(String, String)> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-files", "--stage", "--", path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?;
    let (meta, _) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let mode = parts.next()?.to_string();
    let oid = parts.next()?.to_string();
    Some((mode, oid))
}

fn git_hash_object_path(workdir: &std::path::Path, abs: &std::path::Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .arg("hash-object")
        .arg(abs)
        .output()
        .map_err(|e| Error::Git(format!("hash-object: {e}")))?;
    if !out.status.success() {
        return Err(Error::Git("hash-object failed".into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_hash_object_bytes(workdir: &std::path::Path, bytes: &[u8]) -> Option<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    {
        let stdin = child.stdin.as_mut()?;
        stdin.write_all(bytes).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
