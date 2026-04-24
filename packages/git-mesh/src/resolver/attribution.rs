//! HEAD-source culprit attribution. Blames the commit in `anchor..HEAD`
//! that produced `current.blob`. Only meaningful when the drift `source`
//! is HEAD; non-HEAD drift returns `None`.

use crate::git;
use crate::types::{DriftSource, RangeResolved};
use crate::{Error, Result};

/// Blame the commit in `anchor..HEAD` that produced `current.blob`, when
/// the drift `source` is HEAD (plan §B2). For non-HEAD drift sources or
/// when no blob resolves, return `None`.
pub fn culprit_commit(
    repo: &gix::Repository,
    resolved: &RangeResolved,
) -> Result<Option<String>> {
    if resolved.source != Some(DriftSource::Head) {
        return Ok(None);
    }
    let cur = match resolved.current.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };
    if cur.blob.is_none() {
        return Ok(None);
    }
    let path = cur.path.to_string_lossy().into_owned();
    let head = git::head_oid(repo)?;
    let workdir = git::work_dir(repo)?;
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args([
            "log",
            "-n",
            "1",
            "--format=%H",
            &format!("{}..{}", resolved.anchor_sha, head),
            "--",
            &path,
        ])
        .output()
        .map_err(|e| Error::Git(format!("git log culprit: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
}
