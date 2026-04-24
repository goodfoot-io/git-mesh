//! Engine orchestration: layer setup, per-range resolution, mesh-wide
//! resolution, acknowledgment + pending wiring, concurrency guard.

pub mod pending;
pub(crate) mod range;
pub(crate) mod whole_file;

use super::layers::{
    CustomFilters, LayerDiffs, LfsState, read_conflicted_paths, read_index_layer,
    read_index_trailer, read_worktree_layer,
};
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::types::{
    EngineOptions, LayerSet, MeshResolved, PendingFinding, RangeExtent, RangeLocation,
    RangeResolved, RangeStatus,
};
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use pending::{apply_acknowledgment, build_pending_findings};
use range::resolve_range_inner;

/// Engine-level state cached for one `stale` run.
pub(crate) struct EngineState {
    pub(crate) layers: LayerSet,
    pub(crate) index_diffs: Option<LayerDiffs>,
    pub(crate) worktree_diffs: Option<LayerDiffs>,
    pub(crate) conflicted_paths: HashSet<String>,
    index_trailer_start: Option<[u8; 20]>,
    pub(crate) warnings: Vec<String>,
    pub(crate) lfs: LfsState,
    pub(crate) custom_filters: CustomFilters,
}

impl EngineState {
    fn new(repo: &gix::Repository, layers: LayerSet) -> Result<Self> {
        let index_trailer_start = read_index_trailer(repo).ok();
        let mut s = EngineState {
            layers,
            index_diffs: None,
            worktree_diffs: None,
            conflicted_paths: HashSet::new(),
            index_trailer_start,
            warnings: Vec::new(),
            lfs: None,
            custom_filters: HashMap::new(),
        };
        if layers.index || layers.worktree {
            s.conflicted_paths = read_conflicted_paths(repo)?;
        }
        if layers.index {
            s.index_diffs = Some(read_index_layer(repo, &mut s.warnings)?);
        }
        if layers.worktree {
            s.worktree_diffs = Some(read_worktree_layer(repo, &mut s.warnings)?);
        }
        Ok(s)
    }

    fn finish(self, repo: &gix::Repository) {
        if let Some(start) = self.index_trailer_start
            && let Ok(end) = read_index_trailer(repo)
            && end != start
        {
            eprintln!("warning: index changed during stale; consider re-running");
        }
        for w in self.warnings {
            eprintln!("{w}");
        }
        // Subprocess handles drop here; `FilterProcess`'s `Drop` impl
        // closes stdin (signalling EOF) before waiting on the child.
        let _ = self.lfs;
        let _ = self.custom_filters;
    }
}

pub fn resolve_range(
    repo: &gix::Repository,
    mesh_name: &str,
    range_id: &str,
    options: EngineOptions,
) -> Result<RangeResolved> {
    let mut state = EngineState::new(repo, options.layers)?;
    let mesh = read_mesh(repo, mesh_name)?;
    let mut out = match read_range(repo, range_id) {
        Ok(r) => resolve_range_inner(repo, &mut state, &mesh.config, range_id, r)?,
        Err(Error::RangeNotFound(_)) => orphaned_placeholder(range_id),
        Err(e) => return Err(e),
    };
    if state.layers.staged_mesh {
        apply_acknowledgment(repo, mesh_name, &mut out);
    }
    state.finish(repo);
    Ok(out)
}

pub fn resolve_mesh(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mut state = EngineState::new(repo, options.layers)?;
    let mesh = read_mesh(repo, name)?;
    let mut ranges = Vec::with_capacity(mesh.ranges.len());
    for id in &mesh.ranges {
        match read_range(repo, id) {
            Ok(r) => ranges.push(resolve_range_inner(repo, &mut state, &mesh.config, id, r)?),
            Err(Error::RangeNotFound(_)) => {
                ranges.push(orphaned_placeholder(id));
            }
            Err(e) => return Err(e),
        }
    }
    let pending = if state.layers.staged_mesh {
        for r in &mut ranges {
            apply_acknowledgment(repo, name, r);
        }
        let acked_indices: std::collections::HashSet<usize> = ranges
            .iter()
            .filter_map(|r| r.acknowledged_by.as_ref().map(|s| s.index))
            .collect();
        let mut p = build_pending_findings(repo, name);
        for f in &mut p {
            if let PendingFinding::Add { op, drift, .. } = f {
                let idx = (op.line_number as usize).saturating_sub(1);
                if acked_indices.contains(&idx) {
                    *drift = None;
                }
            }
        }
        p
    } else {
        Vec::new()
    };
    state.finish(repo);
    Ok(MeshResolved {
        name: mesh.name,
        message: mesh.message,
        ranges,
        pending,
    })
}

pub fn stale_meshes(repo: &gix::Repository, options: EngineOptions) -> Result<Vec<MeshResolved>> {
    let names = list_mesh_names(repo)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.push(resolve_mesh(repo, &name, options)?);
    }
    out.sort_by(|a, b| {
        let max_a = a
            .ranges
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(RangeStatus::Fresh);
        let max_b = b
            .ranges
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(RangeStatus::Fresh);
        status_rank(&max_b, &max_a)
    });
    Ok(out)
}

fn status_rank(a: &RangeStatus, b: &RangeStatus) -> std::cmp::Ordering {
    fn rank(s: &RangeStatus) -> u8 {
        match s {
            RangeStatus::Fresh => 0,
            RangeStatus::Moved => 1,
            RangeStatus::Changed => 2,
            RangeStatus::MergeConflict => 3,
            RangeStatus::Submodule => 4,
            RangeStatus::ContentUnavailable(_) => 5,
            RangeStatus::Orphaned => 6,
        }
    }
    rank(a).cmp(&rank(b))
}

fn orphaned_placeholder(range_id: &str) -> RangeResolved {
    RangeResolved {
        range_id: range_id.into(),
        anchor_sha: String::new(),
        anchored: RangeLocation {
            path: PathBuf::new(),
            extent: RangeExtent::Lines { start: 0, end: 0 },
            blob: None,
        },
        current: None,
        status: RangeStatus::Orphaned,
        source: None,
        acknowledged_by: None,
        culprit: None,
    }
}


