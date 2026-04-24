//! Worktree-layer normalized reads. Routes through `gix` for core
//! filters, the custom-driver subprocess for `filter.<name>.process`,
//! and direct `readlink` for symlinks. LFS is intercepted upstream.

use super::filter_process::{
    CustomFilterOutcome, CustomFilters, custom_filter_smudge, is_custom_filter_configured,
};
use crate::git;
use crate::types;
use crate::{Error, Result};

/// Probe `.gitattributes` for a custom `filter=<name>` driver on
/// `path`. Returns `Some(name)` when the driver is unknown — neither on
/// the core-filter allowlist nor backed by a `filter.<name>.process` —
/// i.e. fail-loud short-circuit.
pub(crate) fn filter_short_circuit(repo: &gix::Repository, path: &str) -> Result<Option<String>> {
    let workdir = git::work_dir(repo)?;
    match types::path_filter_attribute(workdir, std::path::Path::new(path))? {
        Some(name) if types::is_core_filter(&name) => Ok(None),
        Some(name) if is_custom_filter_configured(repo, &name) => Ok(None),
        Some(name) => Ok(Some(name)),
        _ => Ok(None),
    }
}

/// Read a worktree file, applying git's clean filter where possible.
/// LFS is *not* handled here; callers branch on `is_lfs_path` first.
pub(crate) fn read_worktree_normalized(
    repo: &gix::Repository,
    custom_filters: &mut CustomFilters,
    rel_path: &str,
) -> Result<Vec<u8>> {
    let workdir = git::work_dir(repo)?;
    if let Some(name) = types::path_filter_attribute(workdir, std::path::Path::new(rel_path))?
        && !types::is_core_filter(&name)
    {
        let abs = workdir.join(rel_path);
        let raw = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(_) => return Ok(Vec::new()),
        };
        return match custom_filter_smudge(custom_filters, workdir, &name, rel_path, &raw) {
            CustomFilterOutcome::Bytes(b) => Ok(b),
            CustomFilterOutcome::FilterFailed => Err(Error::FilterFailed { filter: name }),
        };
    }
    let abs = workdir.join(rel_path);
    let md = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => return Ok(Vec::new()),
    };
    if md.file_type().is_symlink() {
        let target = std::fs::read_link(&abs)?;
        return Ok(target.to_string_lossy().into_owned().into_bytes());
    }
    let file = match std::fs::File::open(&abs) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let pipeline = repo.filter_pipeline(None);
    let Ok((mut pipeline, index)) = pipeline else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(&abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    };
    let outcome = pipeline.convert_to_git(file, std::path::Path::new(rel_path), &index);
    let Ok(outcome) = outcome else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(&abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    };
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
    use std::io::Read;
    let mut out = Vec::new();
    match outcome {
        ToGitOutcome::Unchanged(mut r) => {
            r.read_to_end(&mut out)?;
        }
        ToGitOutcome::Buffer(buf) => out.extend_from_slice(buf),
        ToGitOutcome::Process(mut r) => {
            r.read_to_end(&mut out)?;
        }
    }
    Ok(out)
}
