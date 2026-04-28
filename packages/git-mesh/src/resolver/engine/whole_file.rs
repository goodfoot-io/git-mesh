//! Whole-file resolver: blob-OID equality at the deepest enabled layer
//! per plan §D2. Renames produce `Moved`; symlinks/gitlinks compare by
//! recorded blob/SHA.

use super::EngineState;
use crate::git;
use crate::types::{
    Anchor, AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus, DriftSource, MeshConfig,
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
    anchor_id: &str,
    r: Anchor,
) -> Result<AnchorResolved> {
    let anchored = AnchorLocation {
        path: PathBuf::from(&r.path),
        extent: AnchorExtent::WholeFile,
        blob: oid_from_hex(&r.blob).ok(),
    };
    if !is_commit_reachable(repo, &r.anchor_sha)? {
        return Ok(AnchorResolved {
            anchor_id: anchor_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: AnchorStatus::Orphaned,
            source: None,
            layer_sources: vec![],
            acknowledged_by: None,
            culprit: None,
        });
    }

    let head_sha = git::head_oid(repo)?;
    let workdir = git::work_dir(repo)?;
    let current_path = follow_path_to_head(repo, &r.anchor_sha, &head_sha, &r.path)
        .unwrap_or_else(|| r.path.clone());

    let head_kind_sha = tree_entry_for(repo, &head_sha, &current_path);
    let moved = current_path != r.path;

    // Per-layer blob OIDs for whole-file comparison.
    let head_blob: Option<String> = head_kind_sha.as_ref().map(|(_, sha)| sha.clone());

    let index_blob: Option<String> = if state.layers.index {
        if let Some((_mode, sha)) = index_entry_for(repo, &current_path) {
            Some(sha)
        } else {
            head_blob.clone() // no index entry → same as HEAD
        }
    } else {
        head_blob.clone()
    };

    let worktree_blob: Option<Option<String>> = if state.layers.worktree {
        let abs = workdir.join(&current_path);
        if let Ok(md) = std::fs::symlink_metadata(&abs) {
            if md.file_type().is_symlink() {
                let target = std::fs::read_link(&abs)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let oid = git::hash_blob(target.as_bytes())
                    .ok()
                    .map(|o| o.to_string());
                Some(oid)
            } else if md.file_type().is_file()
                && let Ok(bytes) = std::fs::read(&abs)
                && let Ok(oid) = git::hash_blob(&bytes)
            {
                Some(Some(oid.to_string()))
            } else {
                Some(index_blob.clone())
            }
        } else {
            Some(None) // deleted in worktree
        }
    } else {
        None // worktree layer not enabled
    };

    // The deepest-layer blob determines `current`.
    let current_blob: Option<String> = if let Some(wt) = worktree_blob.as_ref() {
        wt.clone()
    } else {
        index_blob.clone()
    };

    let _ = cfg;
    let status: AnchorStatus;
    let source: Option<DriftSource>;
    let layer_sources: Vec<DriftSource>;

    // Determine which layers independently show drift (blob OID != anchor blob).
    let head_drifts = head_blob.as_deref() != Some(r.blob.as_str());
    let index_drifts = state.layers.index && index_blob.as_deref() != Some(r.blob.as_str());
    let worktree_drifts = state.layers.worktree
        && worktree_blob
            .as_ref()
            .map(|b| b.as_deref() != Some(r.blob.as_str()))
            .unwrap_or(false);

    let deepest = if state.layers.worktree {
        DriftSource::Worktree
    } else if state.layers.index {
        DriftSource::Index
    } else {
        DriftSource::Head
    };

    let cur_blob_oid = current_blob.as_deref().and_then(|s| oid_from_hex(s).ok());
    let current_loc = Some(AnchorLocation {
        path: PathBuf::from(&current_path),
        extent: AnchorExtent::WholeFile,
        blob: cur_blob_oid,
    });
    match current_blob.as_deref() {
        None => {
            status = AnchorStatus::Changed;
            source = Some(deepest);
            layer_sources = vec![deepest];
        }
        Some(cur) if cur == r.blob && moved => {
            status = AnchorStatus::Moved;
            source = Some(deepest);
            // MOVED: single row per design requirement 4.
            layer_sources = vec![deepest];
        }
        Some(cur) if cur == r.blob => {
            status = AnchorStatus::Fresh;
            source = None;
            layer_sources = vec![];
        }
        Some(_) => {
            status = AnchorStatus::Changed;
            source = Some(deepest);
            // Collect all drifting layers in I → W → H order.
            let mut ls: Vec<DriftSource> = Vec::new();
            if index_drifts {
                ls.push(DriftSource::Index);
            }
            if worktree_drifts {
                ls.push(DriftSource::Worktree);
            }
            if head_drifts {
                ls.push(DriftSource::Head);
            }
            layer_sources = if ls.is_empty() { vec![deepest] } else { ls };
        }
    }

    Ok(AnchorResolved {
        anchor_id: anchor_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current: current_loc,
        status,
        source,
        layer_sources,
        acknowledged_by: None,
        culprit: None,
    })
}

/// Walk `anchor..head` (oldest-first), following any rename that renames
/// our currently-tracked path to a new name; return the final path if it
/// differs from the input. This replaces the previous
/// `git log --follow --name-only` subprocess.
///
/// gix has no first-class `--follow` walker, so we walk commits manually
/// and run a tree-vs-first-parent diff per commit with rewrite tracking
/// enabled (50% similarity, the same default `git -M` uses). The first
/// `Rewrite` whose source matches our current path advances the tracked
/// path; commits after the path's deletion (without a paired rename)
/// fall back to the last known name. The result for a single
/// straight-line rename trail is identical to `git log --follow`'s; for
/// pathological copy/rename graphs this is a strictly weaker but
/// well-defined heuristic.
fn follow_path_to_head(
    repo: &gix::Repository,
    anchor: &str,
    head: &str,
    path: &str,
) -> Option<String> {
    let head_id = repo.rev_parse_single(head).ok()?.detach();
    let anchor_id = repo.rev_parse_single(anchor).ok()?.detach();
    let walk = repo
        .rev_walk([head_id])
        .with_hidden([anchor_id])
        .all()
        .ok()?;
    let mut commits: Vec<gix::ObjectId> = Vec::new();
    for info in walk {
        let info = info.ok()?;
        commits.push(info.id);
    }
    commits.reverse(); // oldest-first

    let mut current = path.to_string();
    for commit_id in commits {
        let commit = repo.find_commit(commit_id).ok()?;
        let new_tree = commit.tree().ok()?;
        let parents: Vec<gix::ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        // Mirror `git log --follow`'s heuristic: first parent only.
        let old_tree = match parents.first() {
            Some(pid) => repo.find_commit(*pid).ok()?.tree().ok()?,
            None => repo.empty_tree(),
        };
        let mut opts = gix::diff::Options::default();
        opts.track_rewrites(Some(gix::diff::Rewrites::default()));
        let changes = repo
            .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(opts))
            .ok()?;
        for change in changes {
            use gix::object::tree::diff::ChangeDetached;
            if let ChangeDetached::Rewrite {
                source_location,
                location,
                ..
            } = change
            {
                let src = source_location.to_string();
                if src == current {
                    current = location.to_string();
                    break;
                }
            }
        }
    }

    if current == path { None } else { Some(current) }
}

fn tree_entry_for(repo: &gix::Repository, commit: &str, path: &str) -> Option<(String, String)> {
    let (mode, oid) = git::tree_entry_at(repo, commit, std::path::Path::new(path)).ok()??;
    let mut buf = [0u8; 6];
    let mode_str = mode.as_bytes(&mut buf).to_string();
    Some((mode_str, oid.to_string()))
}

fn index_entry_for(repo: &gix::Repository, path: &str) -> Option<(String, String)> {
    let entries = git::index_entries(repo).ok()?;
    let entry = entries
        .into_iter()
        .find(|e| e.path == path && e.stage == gix::index::entry::Stage::Unconflicted)?;
    let mut buf = [0u8; 6];
    let mode_str = entry.mode.as_bytes(&mut buf).to_string();
    Some((mode_str, entry.oid.to_string()))
}
