//! Whole-file resolver: blob-OID equality at the deepest enabled layer
//! per plan §D2. Renames produce `Moved`; symlinks/gitlinks compare by
//! recorded blob/SHA.

use super::super::session::follow_path_to_head_shared;
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
    if !state.commit_reachable(repo, &r.anchor_sha)? {
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

    if r.anchor_sha == state.head_sha {
        let content_layers_match_head =
            state.clean_layers || (!state.layers.index && !state.layers.worktree);
        if content_layers_match_head
            && let Some(head_blob) = state.head_blob_at(repo, &r.path)?
            && head_blob == r.blob
        {
            return Ok(AnchorResolved {
                anchor_id: anchor_id.into(),
                anchor_sha: r.anchor_sha,
                anchored,
                current: Some(AnchorLocation {
                    path: PathBuf::from(&r.path),
                    extent: AnchorExtent::WholeFile,
                    blob: oid_from_hex(&head_blob).ok(),
                }),
                status: AnchorStatus::Fresh,
                source: None,
                layer_sources: vec![],
                acknowledged_by: None,
                culprit: None,
            });
        }
    }

    let workdir = git::work_dir(repo)?;
    // Phase 2: rename trail consumes per-commit deltas from the shared
    // session instead of running its own `anchor..HEAD` rev_walk.
    let current_path = follow_path_to_head_shared(
        repo,
        &mut state.session,
        &r.anchor_sha,
        &r.path,
        cfg.copy_detection,
        &mut state.warnings,
    )
    .unwrap_or_else(|| r.path.clone());

    let moved = current_path != r.path;

    // Per-layer blob OIDs for whole-file comparison.
    let head_blob: Option<String> = state.head_blob_at(repo, &current_path)?;
    let deepest = if state.layers.worktree {
        DriftSource::Worktree
    } else if state.layers.index {
        DriftSource::Index
    } else {
        DriftSource::Head
    };

    let content_layers_match_head =
        state.clean_layers || (!state.layers.index && !state.layers.worktree);
    if content_layers_match_head
        && let Some(head_blob) = head_blob.as_ref()
        && head_blob == &r.blob
    {
        let status = if moved {
            AnchorStatus::Moved
        } else {
            AnchorStatus::Fresh
        };
        let source = if moved { Some(deepest) } else { None };
        let layer_sources = if moved { vec![deepest] } else { vec![] };
        return Ok(AnchorResolved {
            anchor_id: anchor_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: Some(AnchorLocation {
                path: PathBuf::from(&current_path),
                extent: AnchorExtent::WholeFile,
                blob: oid_from_hex(head_blob).ok(),
            }),
            status,
            source,
            layer_sources,
            acknowledged_by: None,
            culprit: None,
        });
    }

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

fn index_entry_for(repo: &gix::Repository, path: &str) -> Option<(String, String)> {
    let entries = git::index_entries(repo).ok()?;
    let entry = entries
        .into_iter()
        .find(|e| e.path == path && e.stage == gix::index::entry::Stage::Unconflicted)?;
    let mut buf = [0u8; 6];
    let mode_str = entry.mode.as_bytes(&mut buf).to_string();
    Some((mode_str, entry.oid.to_string()))
}
