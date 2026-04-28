//! HEAD-source culprit attribution. Blames the commit in `anchor..HEAD`
//! that produced `current.blob`. Only meaningful when the drift `source`
//! is HEAD; non-HEAD drift returns `None`.

use crate::Result;
use crate::git;
use crate::types::{DriftSource, AnchorResolved};

/// Blame the commit in `anchor..HEAD` that produced `current.blob`, when
/// the drift `source` is HEAD (plan §B2). For non-HEAD drift sources or
/// when no blob resolves, return `None`.
///
/// Slice 9 of the gix migration: the previous `git log -n 1 --format=%H
/// anchor..HEAD -- <path>` subprocess is replaced with a targeted gix
/// rev-walk. We don't need full blame (`gix::blame`) here — the engine
/// only wants the most recent commit in `anchor..HEAD` that touched
/// `path` — so we walk newest-first and stop at the first commit whose
/// tree-vs-first-parent diff mentions the path. This matches `git log`'s
/// default path-filter semantics (no `--follow`) used at the call site.
pub fn culprit_commit(repo: &gix::Repository, resolved: &AnchorResolved) -> Result<Option<String>> {
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
    let head_hex = git::head_oid(repo)?;

    let head_id = match repo.rev_parse_single(head_hex.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => return Ok(None),
    };
    let anchor_id = match repo.rev_parse_single(resolved.anchor_sha.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => return Ok(None),
    };

    let walk = match repo.rev_walk([head_id]).with_hidden([anchor_id]).all() {
        Ok(w) => w,
        Err(_) => return Ok(None),
    };

    let path_bytes = path.as_bytes();
    for info in walk {
        let info = match info {
            Ok(i) => i,
            Err(_) => continue,
        };
        let commit = match repo.find_commit(info.id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let new_tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let parents: Vec<_> = commit.parent_ids().map(|p| p.detach()).collect();
        let old_tree = match parents.first() {
            Some(pid) => match repo.find_commit(*pid) {
                Ok(parent) => parent.tree().unwrap_or_else(|_| repo.empty_tree()),
                Err(_) => repo.empty_tree(),
            },
            None => repo.empty_tree(),
        };
        // Disable rewrite tracking — we only want the path-touched test,
        // not rename pairing (matches `git log -- <path>` defaults).
        let mut opts = gix::diff::Options::default();
        opts.track_rewrites(None);
        let changes = match repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(opts)) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let touches = changes.iter().any(|change| {
            use gix::object::tree::diff::ChangeDetached;
            match change {
                ChangeDetached::Addition { location, .. }
                | ChangeDetached::Deletion { location, .. }
                | ChangeDetached::Modification { location, .. } => {
                    location.as_slice() == path_bytes
                }
                ChangeDetached::Rewrite {
                    source_location,
                    location,
                    ..
                } => source_location.as_slice() == path_bytes || location.as_slice() == path_bytes,
            }
        });
        if touches {
            return Ok(Some(info.id.to_string()));
        }
    }
    Ok(None)
}
