//! Engine orchestration: layer setup, per-anchor resolution, mesh-wide
//! resolution, acknowledgment + pending wiring, concurrency guard.

pub(crate) mod anchor;
pub mod pending;
pub(crate) mod whole_file;

use super::layers::{
    CustomFilters, LayerDiffs, LfsState, filter_short_circuit, read_conflicted_paths,
    read_index_layer, read_index_trailer, read_layer_status, read_worktree_layer,
    read_worktree_layer_for_paths,
};
use super::session::ResolveSession;

use crate::mesh::read::{list_mesh_refs, read_mesh, read_mesh_from_commit};
use crate::types::{
    AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus, EngineOptions, LayerSet,
    MeshResolved, PendingFinding,
};
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anchor::resolve_anchor_inner;
use pending::{apply_acknowledgment, build_pending_findings};

/// Engine-level state cached for one `stale` run.
pub(crate) struct EngineState {
    pub(crate) layers: LayerSet,
    pub(crate) head_sha: String,
    pub(crate) clean_layers: bool,
    pub(crate) index_diffs: Option<LayerDiffs>,
    pub(crate) worktree_diffs: Option<LayerDiffs>,
    pub(crate) conflicted_paths: HashSet<String>,
    index_trailer_start: Option<[u8; 20]>,
    pub(crate) warnings: Vec<String>,
    pub(crate) lfs: LfsState,
    pub(crate) custom_filters: CustomFilters,
    /// Phase 1+2 shared state: one rev-walk per `(repo, anchor_sha)`,
    /// reused across every anchor that pins that commit.
    pub(crate) session: ResolveSession,
    /// Phase 4: when false, `compute_layer_sources` may short-circuit
    /// once it has enough information to drive the exit code. Set by
    /// `cli/stale.rs` based on whether the output mode requires per-layer
    /// detail (`--patch`, `--stat`, the `human` renderer).
    pub(crate) needs_all_layers: bool,
    /// Per-command memo for anchor commit reachability. This avoids
    /// scanning all refs once per anchor in large repositories.
    commit_reachability: HashMap<String, bool>,
    /// Per-command memo for blob OIDs in the current HEAD tree. Many meshes
    /// pin the same paths, so resolving each path once avoids repeated tree
    /// walks without storing anything across invocations.
    head_blobs: HashMap<String, Option<String>>,
}

impl EngineState {
    fn new(repo: &gix::Repository, layers: LayerSet, needs_all_layers: bool) -> Result<Self> {
        let _perf = crate::perf::span("resolver.init-layers");
        let head_sha = crate::git::head_oid(repo)?;
        let layer_status = if layers.index || layers.worktree {
            let _perf = crate::perf::span("resolver.init-layers.status");
            read_layer_status(repo).ok()
        } else {
            None
        };
        let clean_layers = layer_status
            .as_ref()
            .is_some_and(|status| status.is_clean());
        let index_trailer_start = read_index_trailer(repo).ok();
        let mut s = EngineState {
            layers,
            head_sha,
            clean_layers,
            index_diffs: None,
            worktree_diffs: None,
            conflicted_paths: HashSet::new(),
            index_trailer_start,
            warnings: Vec::new(),
            lfs: None,
            custom_filters: HashMap::new(),
            session: ResolveSession::new(),
            needs_all_layers,
            commit_reachability: HashMap::new(),
            head_blobs: HashMap::new(),
        };
        if clean_layers {
            if layers.index {
                s.index_diffs = Some(LayerDiffs::empty());
            }
            if layers.worktree {
                s.worktree_diffs = Some(LayerDiffs::empty());
            }
        } else if layers.index || layers.worktree {
            match layer_status.as_ref() {
                Some(status) if !status.requires_full_scan => {
                    if status.has_unmerged {
                        let _perf = crate::perf::span("resolver.init-layers.read-conflicts");
                        s.conflicted_paths = read_conflicted_paths(repo)?;
                    }
                    if layers.index {
                        if status.index_dirty {
                            let _perf = crate::perf::span("resolver.init-layers.read-index-layer");
                            s.index_diffs = Some(read_index_layer(repo, &mut s.warnings)?);
                        } else {
                            s.index_diffs = Some(LayerDiffs::empty());
                        }
                    }
                    if layers.worktree {
                        if status.worktree_paths.is_empty() {
                            s.worktree_diffs = Some(LayerDiffs::empty());
                        } else {
                            let _perf =
                                crate::perf::span("resolver.init-layers.read-worktree-layer");
                            s.worktree_diffs = Some(read_worktree_layer_for_paths(
                                repo,
                                &status.worktree_paths,
                                &mut s.warnings,
                            )?);
                        }
                    }
                }
                _ => {
                    let _perf = crate::perf::span("resolver.init-layers.full-scan");
                    s.conflicted_paths = read_conflicted_paths(repo)?;
                    if layers.index {
                        s.index_diffs = Some(read_index_layer(repo, &mut s.warnings)?);
                    }
                    if layers.worktree {
                        s.worktree_diffs = Some(read_worktree_layer(repo, &mut s.warnings)?);
                    }
                }
            }
        }
        Ok(s)
    }

    pub(crate) fn commit_reachable(
        &mut self,
        repo: &gix::Repository,
        commit: &str,
    ) -> Result<bool> {
        if commit == self.head_sha {
            self.commit_reachability.insert(commit.to_string(), true);
            return Ok(true);
        }
        if let Some(reachable) = self.commit_reachability.get(commit) {
            return Ok(*reachable);
        }
        let reachable = crate::git::commit_reachable_from_any_ref(repo, commit)?;
        self.commit_reachability
            .insert(commit.to_string(), reachable);
        Ok(reachable)
    }

    pub(crate) fn head_blob_at(
        &mut self,
        repo: &gix::Repository,
        path: &str,
    ) -> Result<Option<String>> {
        if let Some(blob) = self.head_blobs.get(path) {
            return Ok(blob.clone());
        }
        let blob = match crate::git::path_blob_at(repo, &self.head_sha, path) {
            Ok(blob) => Some(blob),
            Err(Error::PathNotInTree { .. }) => None,
            Err(e) => return Err(e),
        };
        self.head_blobs.insert(path.to_string(), blob.clone());
        Ok(blob)
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

pub fn resolve_anchor(
    repo: &gix::Repository,
    mesh_name: &str,
    anchor_id: &str,
    options: EngineOptions,
) -> Result<AnchorResolved> {
    let _perf = crate::perf::span("resolver.resolve-anchor");
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    let mesh = read_mesh(repo, mesh_name)?;
    let mut out = match mesh.anchors_v2.into_iter().find(|(id, _)| id == anchor_id) {
        Some((_, r)) => resolve_anchor_inner(repo, &mut state, &mesh.config, anchor_id, r)?,
        None => orphaned_placeholder(anchor_id),
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
    let _perf = crate::perf::span("resolver.resolve-mesh");
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    let out = resolve_mesh_with_state(repo, &mut state, name, options)?;
    state.finish(repo);
    Ok(out)
}

/// Resolve a mesh against the anchors stored at a specific mesh-ref commit.
///
/// Compaction uses this to keep the resolver's view consistent with the
/// `current_tip` it captured for the CAS expected-old-oid. Without this,
/// if the live ref drifts between read and classification, anchor data
/// comes from a different commit than the CAS guard expects.
pub fn resolve_mesh_at(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
    commit_oid: &str,
) -> Result<MeshResolved> {
    let _perf = crate::perf::span("resolver.resolve-mesh-at");
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    let out = resolve_mesh_with_state_at(repo, &mut state, name, commit_oid, options)?;
    state.finish(repo);
    Ok(out)
}

fn resolve_mesh_with_state(
    repo: &gix::Repository,
    state: &mut EngineState,
    name: &str,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mesh = {
        let _perf = crate::perf::span("resolver.read-mesh");
        read_mesh(repo, name)?
    };
    resolve_loaded_mesh_with_state(repo, state, mesh, options)
}

fn resolve_mesh_with_state_at(
    repo: &gix::Repository,
    state: &mut EngineState,
    name: &str,
    commit_oid: &str,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mesh = {
        let _perf = crate::perf::span("resolver.read-mesh");
        read_mesh_from_commit(repo, name, commit_oid)?
    };
    resolve_loaded_mesh_with_state(repo, state, mesh, options)
}

fn resolve_loaded_mesh_with_state(
    repo: &gix::Repository,
    state: &mut EngineState,
    mesh: crate::types::Mesh,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mut anchors = Vec::with_capacity(mesh.anchors_v2.len());
    let mut filtered_by_since: usize = 0;
    {
        let _perf = crate::perf::span("resolver.resolve-anchors");
        for (id, r) in mesh.anchors_v2 {
            if let Some(since_oid) = options.since
                && !anchor_at_or_after(repo, &r.anchor_sha, since_oid)
            {
                filtered_by_since += 1;
                continue;
            }
            anchors.push(resolve_anchor_inner(
                repo,
                &mut *state,
                &mesh.config,
                &id,
                r,
            )?);
        }
    }
    if filtered_by_since > 0
        && let Some(since_oid) = options.since
    {
        state.warnings.push(format!(
            "filtered {filtered_by_since} anchors anchored before {}",
            since_oid
        ));
    }
    let pending = if state.layers.staged_mesh {
        let _perf = crate::perf::span("resolver.resolve-pending");
        {
            for r in &mut anchors {
                apply_acknowledgment(repo, &mesh.name, r);
            }
            let acked_indices: std::collections::HashSet<usize> = anchors
                .iter()
                .filter_map(|r| r.acknowledged_by.as_ref().map(|s| s.index))
                .collect();
            let mut p = build_pending_findings(repo, &mesh.name);
            for f in &mut p {
                if let PendingFinding::Add { op, drift, .. } = f {
                    let idx = (op.line_number as usize).saturating_sub(1);
                    if acked_indices.contains(&idx) {
                        *drift = None;
                    }
                }
            }
            p
        }
    } else {
        Vec::new()
    };
    Ok(MeshResolved {
        name: mesh.name,
        message: mesh.message,
        anchors,
        pending,
    })
}

fn mesh_is_reportable_in_stale_discovery(m: &MeshResolved) -> bool {
    m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh) || !m.pending.is_empty()
}

/// Resolve every mesh in the repository, sorted worst-status-first.
/// Used by the advice engine, which needs routing context for all meshes
/// regardless of drift state.
pub(crate) fn all_meshes(
    repo: &gix::Repository,
    options: EngineOptions,
) -> Result<Vec<MeshResolved>> {
    let mesh_refs = {
        let _perf = crate::perf::span("resolver.list-meshes");
        list_mesh_refs(repo)?
    };
    let mut out = Vec::with_capacity(mesh_refs.len());
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    {
        let _perf = crate::perf::span("resolver.resolve-meshes");
        for (name, commit_oid) in mesh_refs {
            out.push(resolve_mesh_with_state_at(
                repo,
                &mut state,
                &name,
                &commit_oid,
                options,
            )?);
        }
    }
    state.finish(repo);
    if out.len() > 1 {
        sort_meshes_worst_first(&mut out);
    }
    Ok(out)
}

pub fn stale_meshes(repo: &gix::Repository, options: EngineOptions) -> Result<Vec<MeshResolved>> {
    let mesh_refs = {
        let _perf = crate::perf::span("resolver.list-meshes");
        list_mesh_refs(repo)?
    };
    let mut out = Vec::new();
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    {
        let _perf = crate::perf::span("resolver.resolve-stale-meshes");
        for (name, commit_oid) in mesh_refs {
            let mesh = {
                let _perf = crate::perf::span("resolver.read-mesh");
                read_mesh_from_commit(repo, &name, &commit_oid)?
            };
            if can_skip_clean_head_pinned_mesh(repo, &mut state, &name, &mesh, options)? {
                continue;
            }
            let resolved = resolve_loaded_mesh_with_state(repo, &mut state, mesh, options)?;
            if mesh_is_reportable_in_stale_discovery(&resolved) {
                out.push(resolved);
            }
        }
    }
    state.finish(repo);
    if out.len() > 1 {
        sort_meshes_worst_first(&mut out);
    }
    Ok(out)
}

pub(crate) fn resolve_meshes_in_order(
    repo: &gix::Repository,
    names: &[String],
    options: EngineOptions,
) -> Result<Vec<(String, std::result::Result<MeshResolved, Error>)>> {
    let committed_refs: HashMap<String, String> = list_mesh_refs(repo)?.into_iter().collect();
    let mut out = Vec::with_capacity(names.len());
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    {
        let _perf = crate::perf::span("resolver.resolve-meshes");
        for name in names {
            let resolved = if let Some(commit_oid) = committed_refs.get(name) {
                resolve_mesh_with_state_at(repo, &mut state, name, commit_oid, options)
            } else {
                resolve_mesh_with_state(repo, &mut state, name, options)
            };
            out.push((name.clone(), resolved));
        }
    }
    state.finish(repo);
    Ok(out)
}

fn status_rank(a: &AnchorStatus, b: &AnchorStatus) -> std::cmp::Ordering {
    fn rank(s: &AnchorStatus) -> u8 {
        match s {
            AnchorStatus::Fresh => 0,
            AnchorStatus::Moved => 1,
            AnchorStatus::Changed => 2,
            AnchorStatus::MergeConflict => 3,
            AnchorStatus::Submodule => 4,
            AnchorStatus::ContentUnavailable(_) => 5,
            AnchorStatus::Orphaned => 6,
        }
    }
    rank(a).cmp(&rank(b))
}

fn sort_meshes_worst_first(meshes: &mut [MeshResolved]) {
    let _perf = crate::perf::span("resolver.sort-meshes");
    meshes.sort_by(|a, b| {
        let max_a = a
            .anchors
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(AnchorStatus::Fresh);
        let max_b = b
            .anchors
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(AnchorStatus::Fresh);
        status_rank(&max_b, &max_a)
    });
}

fn can_skip_clean_head_pinned_mesh(
    repo: &gix::Repository,
    state: &mut EngineState,
    name: &str,
    mesh: &crate::types::Mesh,
    options: EngineOptions,
) -> Result<bool> {
    if options.since.is_some() {
        return Ok(false);
    }
    if !content_layers_are_head_authoritative(state) {
        return Ok(false);
    }
    if mesh_has_staged_state(repo, name) {
        return Ok(false);
    }
    for (_, anchor) in &mesh.anchors_v2 {
        if anchor.anchor_sha != state.head_sha {
            return Ok(false);
        }
        if filter_short_circuit(repo, &anchor.path)?.is_some() {
            return Ok(false);
        }
        let Some(head_blob) = state.head_blob_at(repo, &anchor.path)? else {
            return Ok(false);
        };
        if head_blob != anchor.blob {
            return Ok(false);
        }
    }
    Ok(true)
}

fn content_layers_are_head_authoritative(state: &EngineState) -> bool {
    state.clean_layers || (!state.layers.index && !state.layers.worktree)
}

fn mesh_has_staged_state(repo: &gix::Repository, name: &str) -> bool {
    crate::staging::read_staged_ops(repo, name).is_ok_and(|ops| !ops.is_empty())
}

/// Slice 5: returns true when the anchor should pass the `--since`
/// filter. The semantic is "anchored at or after `since`" — i.e.
/// `since` is an ancestor of (or equal to) `anchor_sha`. Anchors that
/// don't parse / aren't reachable fall through as `true` (orphans are
/// not hidden by `--since`).
fn anchor_at_or_after(repo: &gix::Repository, anchor_sha: &str, since: gix::ObjectId) -> bool {
    use std::str::FromStr;
    let Ok(anchor_id) = gix::ObjectId::from_str(anchor_sha) else {
        return true;
    };
    if anchor_id == since {
        return true;
    }
    match repo.merge_base(anchor_id, since) {
        Ok(base) => base.detach() == since,
        Err(_) => true,
    }
}

fn orphaned_placeholder(anchor_id: &str) -> AnchorResolved {
    AnchorResolved {
        anchor_id: anchor_id.into(),
        anchor_sha: String::new(),
        anchored: AnchorLocation {
            path: PathBuf::new(),
            extent: AnchorExtent::LineRange { start: 0, end: 0 },
            blob: None,
        },
        current: None,
        status: AnchorStatus::Orphaned,
        source: None,
        layer_sources: vec![],
        acknowledged_by: None,
        culprit: None,
    }
}
