//! Fetch/push for mesh and range refs (§7).

use crate::git::{git_stdout, git_stdout_lines, git_stdout_optional, work_dir};
use crate::{Error, Result};
use std::path::Path;

const REFSPECS: [&str; 2] = [
    "+refs/ranges/*:refs/ranges/*",
    "+refs/meshes/*:refs/meshes/*",
];

pub fn default_remote(repo: &gix::Repository) -> Result<String> {
    let wd = work_dir(repo)?;
    Ok(git_stdout_optional(wd, ["config", "--get", "mesh.defaultRemote"])?
        .unwrap_or_else(|| "origin".to_string()))
}

pub fn fetch_mesh_refs(repo: &gix::Repository, remote: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    ensure_refspec_configured_inner(wd, remote)?;
    git_stdout(wd, ["fetch", remote])?;
    Ok(())
}

pub fn push_mesh_refs(repo: &gix::Repository, remote: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    ensure_refspec_configured_inner(wd, remote)?;
    git_stdout(wd, ["push", remote])?;
    Ok(())
}

pub fn ensure_refspec_configured(repo: &gix::Repository, remote: &str) -> Result<()> {
    ensure_refspec_configured_inner(work_dir(repo)?, remote)
}

fn ensure_refspec_configured_inner(wd: &Path, remote: &str) -> Result<()> {
    // Fail-closed: remote must exist before we add config lines.
    let url = git_stdout_optional(wd, ["config", "--get", &format!("remote.{remote}.url")])?;
    if url.is_none() {
        return Err(Error::RefspecMissing {
            remote: remote.into(),
        });
    }
    let fetch_key = format!("remote.{remote}.fetch");
    let push_key = format!("remote.{remote}.push");
    let existing_fetch = git_stdout_lines(wd, ["config", "--get-all", &fetch_key])?;
    let existing_push = git_stdout_lines(wd, ["config", "--get-all", &push_key])?;
    for rs in REFSPECS {
        if !existing_fetch.iter().any(|e| e == rs) {
            git_stdout(wd, ["config", "--add", &fetch_key, rs])?;
        }
        if !existing_push.iter().any(|e| e == rs) {
            git_stdout(wd, ["config", "--add", &push_key, rs])?;
        }
    }
    Ok(())
}
