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
        return Err(Error::RemoteNotFound {
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
    let path = crate::git::common_dir(repo).join("config");
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

/// Slice 6c: collapse duplicate mesh refspecs in
/// `remote.<remote>.{fetch,push}`. Returns `(fetch_dupes, push_dupes)`
/// counts that were dropped. Idempotent — running again yields `(0, 0)`.
///
/// We rewrite both lists in place by reading every value, deduping while
/// preserving first-occurrence order, then re-writing the section's
/// `fetch` / `push` keys. Non-mesh refspecs (e.g. the default
/// `+refs/heads/*:refs/remotes/<remote>/*`) are preserved.
pub fn dedupe_mesh_refspecs(repo: &gix::Repository, remote: &str) -> Result<(usize, usize)> {
    if get_remote_url(repo, remote).is_none() {
        return Ok((0, 0));
    }
    let fetch = get_remote_multi(repo, remote, "fetch");
    let push = get_remote_multi(repo, remote, "push");
    let (new_fetch, fetch_dupes) = dedupe_preserve(&fetch);
    let (new_push, push_dupes) = dedupe_preserve(&push);
    if fetch_dupes == 0 && push_dupes == 0 {
        return Ok((0, 0));
    }

    let path = crate::git::common_dir(repo).join("config");
    let mut file =
        gix::config::File::from_path_no_includes(path.clone(), gix::config::Source::Local)
            .map_err(|e| Error::Git(format!("load config: {e}")))?;
    let subsection: &gix::bstr::BStr = remote.as_bytes().into();
    // Remove all existing fetch / push keys in the remote section, then
    // re-add the deduped lists. `section.clear` drops every value at
    // every key, so we capture other keys first and re-emit them.
    let mut section = file
        .section_mut_or_create_new("remote", Some(subsection))
        .map_err(|e| Error::Git(format!("section: {e}")))?;
    if fetch_dupes > 0 {
        // `SectionMut::remove` returns one occurrence at a time; loop
        // until no more exist, then re-emit the deduped list.
        while section.remove("fetch").is_some() {}
        for v in &new_fetch {
            section.push(
                "fetch"
                    .try_into()
                    .map_err(|e| Error::Git(format!("key: {e}")))?,
                Some(v.as_bytes().into()),
            );
        }
    }
    if push_dupes > 0 {
        while section.remove("push").is_some() {}
        for v in &new_push {
            section.push(
                "push"
                    .try_into()
                    .map_err(|e| Error::Git(format!("key: {e}")))?,
                Some(v.as_bytes().into()),
            );
        }
    }
    let bytes = file.to_bstring();
    std::fs::write(&path, bytes.as_slice())
        .map_err(|e| Error::Git(format!("write config: {e}")))?;
    Ok((fetch_dupes, push_dupes))
}

fn dedupe_preserve(values: &[String]) -> (Vec<String>, usize) {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    let mut dupes = 0usize;
    for v in values {
        if seen.insert(v.clone()) {
            out.push(v.clone());
        } else {
            dupes += 1;
        }
    }
    (out, dupes)
}

#[cfg(test)]
mod tests {
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
            String::from_utf8_lossy(&out.stderr),
        );
    }

    fn seed() -> (tempfile::TempDir, gix::Repository) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        run_git(dir, &["remote", "add", "origin", "/tmp/__git_mesh_fake__"]);
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    #[test]
    fn ensure_refspec_configured_is_idempotent() {
        let (td, repo) = seed();
        ensure_refspec_configured(&repo, "origin").unwrap();
        // Re-open to see the freshly-written config.
        let repo2 = gix::open(td.path()).unwrap();
        ensure_refspec_configured(&repo2, "origin").unwrap();
        let repo3 = gix::open(td.path()).unwrap();
        let fetch = get_remote_multi(&repo3, "origin", "fetch");
        let push = get_remote_multi(&repo3, "origin", "push");
        let mesh_fetch_count = fetch
            .iter()
            .filter(|s| s.contains("refs/ranges/") || s.contains("refs/meshes/"))
            .count();
        let mesh_push_count = push
            .iter()
            .filter(|s| s.contains("refs/ranges/") || s.contains("refs/meshes/"))
            .count();
        assert_eq!(
            mesh_fetch_count, 2,
            "fetch refspecs not idempotent: {fetch:?}"
        );
        assert_eq!(mesh_push_count, 2, "push refspecs not idempotent: {push:?}");
    }

    #[test]
    fn dedupe_mesh_refspecs_collapses_duplicates() {
        let (td, _repo) = seed();
        // Manually inject duplicates by appending lines to .git/config.
        let cfg = td.path().join(".git/config");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str(
            "\tfetch = +refs/ranges/*:refs/ranges/*\n\
             \tfetch = +refs/ranges/*:refs/ranges/*\n\
             \tfetch = +refs/meshes/*:refs/meshes/*\n\
             \tpush = +refs/meshes/*:refs/meshes/*\n\
             \tpush = +refs/meshes/*:refs/meshes/*\n",
        );
        std::fs::write(&cfg, text).unwrap();
        let repo = gix::open(td.path()).unwrap();
        let (fd, pd) = dedupe_mesh_refspecs(&repo, "origin").unwrap();
        assert_eq!(fd, 1);
        assert_eq!(pd, 1);
        let repo2 = gix::open(td.path()).unwrap();
        // Idempotent second pass.
        let (fd2, pd2) = dedupe_mesh_refspecs(&repo2, "origin").unwrap();
        assert_eq!((fd2, pd2), (0, 0));
        let fetch = get_remote_multi(&repo2, "origin", "fetch");
        let push = get_remote_multi(&repo2, "origin", "push");
        assert_eq!(
            fetch
                .iter()
                .filter(|s| s.as_str() == "+refs/ranges/*:refs/ranges/*")
                .count(),
            1
        );
        assert_eq!(
            push.iter()
                .filter(|s| s.as_str() == "+refs/meshes/*:refs/meshes/*")
                .count(),
            1
        );
    }
}
