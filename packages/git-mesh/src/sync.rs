//! Fetch/push for mesh and range refs (§7).
//!
//! Remote `fetch` and `push` are still performed via the `git` subprocess
//! because gix 0.81 requires the `blocking-network-client` +
//! `blocking-http-transport-*` feature stack and does not provide a
//! credential-helper integration equivalent to `git`'s for all the
//! transports (SSH agent + Windows credential manager + keychain). This
//! is the sole remaining `Command::new("git")` call site in the
//! production path.
//!
//! Everything else — reading config values, writing refspecs, resolving
//! the remote URL — goes through `gix`.

use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

const REFSPECS: [&str; 2] = [
    "+refs/ranges/*:refs/ranges/*",
    "+refs/meshes/*:refs/meshes/*",
];

fn work_dir(repo: &gix::Repository) -> Result<&Path> {
    crate::git::work_dir(repo)
}

/// Read a scalar config value (first match wins).
pub(crate) fn get_remote_url(repo: &gix::Repository, remote: &str) -> Option<String> {
    let key = format!("remote.{remote}.url");
    repo.config_snapshot()
        .string(key.as_str())
        .map(|v| v.to_string())
}

/// Read every value of a multi-valued remote key like `fetch` or `push`.
pub(crate) fn get_remote_multi(repo: &gix::Repository, remote: &str, sub: &str) -> Vec<String> {
    let snap = repo.config_snapshot();
    let file = snap.plumbing();
    let mut out = Vec::new();
    let remote_bstr: &gix::bstr::BStr = remote.as_bytes().into();
    if let Some(values) = file.strings_by("remote", Some(remote_bstr), sub) {
        for v in values {
            out.push(v.to_string());
        }
    }
    out
}

pub fn default_remote(repo: &gix::Repository) -> Result<String> {
    Ok(repo
        .config_snapshot()
        .string("mesh.defaultRemote")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "origin".to_string()))
}

pub fn fetch_mesh_refs(repo: &gix::Repository, remote: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    ensure_refspec_configured(repo, remote)?;
    run_git(wd, &["fetch", remote])
}

pub fn push_mesh_refs(repo: &gix::Repository, remote: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    ensure_refspec_configured(repo, remote)?;
    run_git(wd, &["push", remote])
}

fn run_git(wd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(wd)
        .args(args)
        .output()
        .map_err(|e| Error::Git(format!("spawn git: {e}")))?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

pub fn ensure_refspec_configured(repo: &gix::Repository, remote: &str) -> Result<()> {
    // Fail-closed: remote must exist before we add config lines.
    if get_remote_url(repo, remote).is_none() {
        return Err(Error::RefspecMissing {
            remote: remote.into(),
        });
    }
    let existing_fetch = get_remote_multi(repo, remote, "fetch");
    let existing_push = get_remote_multi(repo, remote, "push");
    let need_fetch: Vec<&str> = REFSPECS
        .iter()
        .copied()
        .filter(|rs| !existing_fetch.iter().any(|e| e == rs))
        .collect();
    let need_push: Vec<&str> = REFSPECS
        .iter()
        .copied()
        .filter(|rs| !existing_push.iter().any(|e| e == rs))
        .collect();
    if need_fetch.is_empty() && need_push.is_empty() {
        return Ok(());
    }

    // Write to `.git/config` directly.
    let wd = work_dir(repo)?;
    let path = wd.join(".git").join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| Error::Git(format!("load config: {e}")))?;
    let subsection: &gix::bstr::BStr = remote.as_bytes().into();
    let mut section = file
        .section_mut_or_create_new("remote", Some(subsection))
        .map_err(|e| Error::Git(format!("section: {e}")))?;
    for rs in &need_fetch {
        section.push(
            "fetch"
                .try_into()
                .map_err(|e| Error::Git(format!("key: {e}")))?,
            Some(rs.as_bytes().into()),
        );
    }
    for rs in &need_push {
        section.push(
            "push"
                .try_into()
                .map_err(|e| Error::Git(format!("key: {e}")))?,
            Some(rs.as_bytes().into()),
        );
    }
    let bytes = file.to_bstring();
    std::fs::write(&path, bytes.as_slice())
        .map_err(|e| Error::Git(format!("write config: {e}")))?;
    Ok(())
}
