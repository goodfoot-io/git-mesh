//! Engine orchestration: layer setup, per-anchor resolution, mesh-wide
//! resolution, acknowledgment + pending wiring, concurrency guard.

pub(crate) mod anchor;
pub mod pending;
pub(crate) mod whole_file;

use super::layers::{
    CustomFilters, LayerDiffs, LfsState, is_custom_filter_configured, read_conflicted_paths,
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
    /// Per-command memo for `.gitattributes` filter-driver lookups, keyed
    /// by `rel_path`. The workdir is constant per `EngineState`, so the
    /// repo handle is implicit. A cached `None` means "no driver / fail
    /// closed" (matches the pre-memo behavior on plumbing error).
    filter_attrs: HashMap<String, Option<String>>,
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
            session: ResolveSession::new(repo),
            needs_all_layers,
            commit_reachability: HashMap::new(),
            head_blobs: HashMap::new(),
            filter_attrs: HashMap::new(),
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
        // HEAD-relative: per drift-label spec, "orphaned (no sha)" applies
        // when the anchor commit is not in HEAD's history, even if another
        // ref still keeps it alive (e.g. after `checkout --orphan`).
        let reachable = crate::git::commit_reachable_from_head(repo, commit)?;
        self.commit_reachability
            .insert(commit.to_string(), reachable);
        Ok(reachable)
    }

    /// Probe `.gitattributes` for a custom `filter=<name>` driver on
    /// `path`, returning `Some(name)` when the driver is unknown
    /// (fail-loud short-circuit). Memoized per-`EngineState`: the first
    /// query for a path performs the full `attr_for` lookup; later
    /// queries are O(1) `HashMap` reads. Reuses the caller's repo handle
    /// instead of re-opening per call.
    pub(crate) fn filter_short_circuit(
        &mut self,
        repo: &gix::Repository,
        path: &str,
    ) -> Result<Option<String>> {
        let name = match self.filter_attribute_value(repo, path)? {
            Some(n) => n,
            None => return Ok(None),
        };
        if crate::types::is_core_filter(&name) {
            return Ok(None);
        }
        if is_custom_filter_configured(repo, &name) {
            return Ok(None);
        }
        Ok(Some(name))
    }

    fn filter_attribute_value(
        &mut self,
        repo: &gix::Repository,
        path: &str,
    ) -> Result<Option<String>> {
        if let Some(cached) = self.filter_attrs.get(path) {
            self.session.filter_attr_hits += 1;
            return Ok(cached.clone());
        }
        self.session.filter_attr_misses += 1;
        // Fail-closed: any plumbing error caches `None` so subsequent
        // reads of the same path return the same answer (matches the
        // un-memoed behavior in `path_filter_attribute_with_repo`).
        let value = crate::types::path_filter_attribute_with_repo(
            repo,
            std::path::Path::new(path),
        )
        .unwrap_or(None);
        self.filter_attrs.insert(path.to_string(), value.clone());
        Ok(value)
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

/// Opaque handle that lets callers outside this module reuse a single
/// `EngineState` across multiple mesh resolutions. Used by the
/// all-mesh `stale --compact` batch path so anchor-history walks and
/// HEAD blob lookups are cached for the whole run.
pub(crate) struct EngineStateHandle(EngineState);

pub(crate) fn new_engine_state(
    repo: &gix::Repository,
    options: EngineOptions,
) -> Result<EngineStateHandle> {
    Ok(EngineStateHandle(EngineState::new(
        repo,
        options.layers,
        options.needs_all_layers,
    )?))
}

impl EngineStateHandle {
    pub(crate) fn head_sha(&self) -> &str {
        &self.0.head_sha
    }

    pub(crate) fn head_blob_at(
        &mut self,
        repo: &gix::Repository,
        path: &str,
    ) -> Result<Option<String>> {
        self.0.head_blob_at(repo, path)
    }

    pub(crate) fn filter_short_circuit(
        &mut self,
        repo: &gix::Repository,
        path: &str,
    ) -> Result<Option<String>> {
        self.0.filter_short_circuit(repo, path)
    }
}

pub(crate) fn resolve_loaded_mesh_with_engine_state(
    repo: &gix::Repository,
    handle: &mut EngineStateHandle,
    mesh: crate::types::Mesh,
    options: EngineOptions,
) -> Result<MeshResolved> {
    resolve_loaded_mesh_with_state(repo, &mut handle.0, mesh, options)
}

/// Resolve a mesh against the anchors stored at a specific mesh-ref
/// commit, reusing the caller's shared `EngineStateHandle`.
///
/// Used by the batch compact CAS-conflict retry path so the per-mesh
/// retry can keep the HEAD-blob cache warmed by the earlier batch
/// classification, instead of throwing away that cache and rebuilding
/// an `EngineState` from scratch.
pub(crate) fn resolve_mesh_at_with_engine_state(
    repo: &gix::Repository,
    handle: &mut EngineStateHandle,
    name: &str,
    options: EngineOptions,
    commit_oid: &str,
) -> Result<MeshResolved> {
    let _perf = crate::perf::span("resolver.resolve-mesh-at-with-engine-state");
    resolve_mesh_with_state_at(repo, &mut handle.0, name, commit_oid, options)
}

fn resolve_loaded_mesh_with_state(
    repo: &gix::Repository,
    state: &mut EngineState,
    mesh: crate::types::Mesh,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mut anchors = Vec::with_capacity(mesh.anchors_v2.len());
    let mut filtered_by_since: usize = 0;
    // Prebuild the candidate-path union per anchor commit so each grouped
    // walk skips commits that don't touch any path the mesh's anchors
    // actually care about. Anchors filtered by `--since` are excluded
    // from the union — they won't be resolved, so their paths shouldn't
    // widen the walk.
    {
        let _perf = crate::perf::span("resolver.prepare-groups");
        let mut by_anchor_sha: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for (_id, r) in &mesh.anchors_v2 {
            if let Some(since_oid) = options.since
                && !anchor_at_or_after(repo, &r.anchor_sha, since_oid)
            {
                continue;
            }
            by_anchor_sha
                .entry(r.anchor_sha.clone())
                .or_default()
                .insert(r.path.clone());
        }
        for (anchor_sha, paths) in &by_anchor_sha {
            state.session.prepare_group(
                repo,
                anchor_sha,
                mesh.config.copy_detection,
                paths,
                &mut state.warnings,
            )?;
        }
    }
    {
        let _perf = crate::perf::span("resolver.resolve-anchors");
        for (id, r) in mesh.anchors_v2 {
            if let Some(since_oid) = options.since
                && !anchor_at_or_after(repo, &r.anchor_sha, since_oid)
            {
                filtered_by_since += 1;
                continue;
            }
            let anchor_t0 = std::time::Instant::now();
            let trace_anchor_sha = r.anchor_sha.clone();
            let trace_path = r.path.clone();
            let fast_path_before = state.session.anchors_fast_path_hits;
            let mut resolved = resolve_anchor_inner(
                repo,
                &mut *state,
                &mesh.config,
                &id,
                r,
            )?;
            let wall_us = anchor_t0.elapsed().as_micros();
            state.session.per_anchor_us.push(wall_us);
            tally_anchor_status(&mut state.session, &resolved.status);
            if let Some(trace) = state.session.per_anchor_trace.as_mut() {
                trace.push(crate::perf::TraceRow {
                    mesh: mesh.name.clone(),
                    anchor_id: id.clone(),
                    anchor_sha: trace_anchor_sha,
                    path: trace_path,
                    wall_us,
                    fast_path: state.session.anchors_fast_path_hits > fast_path_before,
                    status: status_label(&resolved.status),
                });
            }
            populate_drift_locus(repo, &mut resolved, &mut state.session, mesh.config.copy_detection);
            anchors.push(resolved);
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

/// Populate `AnchorResolved.locus` for anchors whose drift is attributed to
/// the HEAD layer or whose status is `Orphaned`. For all other states the
/// per-layer label (worktree / index) suffices and no walk is needed.
fn populate_drift_locus(
    repo: &gix::Repository,
    resolved: &mut AnchorResolved,
    session: &mut super::session::ResolveSession,
    copy_detection: crate::types::CopyDetection,
) {
    use crate::types::{DriftLocus, DriftSource};
    match resolved.status {
        AnchorStatus::Changed if resolved.source == Some(DriftSource::Head) => {
            if let Ok(locus) = super::attribution::drift_locus(repo, resolved, session, copy_detection) {
                resolved.locus = locus;
            }
        }
        AnchorStatus::Orphaned if resolved.locus.is_none() => {
            // Ask the walk to describe an orphaning commit when the anchor
            // is reachable but the path is absent from HEAD; otherwise fall
            // back to `Unreachable`.
            match super::attribution::drift_locus(repo, resolved, session, copy_detection) {
                Ok(Some(locus)) => resolved.locus = Some(locus),
                _ => resolved.locus = Some(DriftLocus::Unreachable),
            }
        }
        _ => {}
    }
}

fn tally_anchor_status(session: &mut super::session::ResolveSession, status: &AnchorStatus) {
    match status {
        AnchorStatus::Fresh => session.anchors_fresh += 1,
        AnchorStatus::Moved => session.anchors_moved += 1,
        AnchorStatus::Changed => session.anchors_changed += 1,
        AnchorStatus::Orphaned => session.anchors_orphaned += 1,
        AnchorStatus::MergeConflict => session.anchors_merge_conflict += 1,
        AnchorStatus::Submodule => session.anchors_unavailable += 1,
        AnchorStatus::ContentUnavailable(_) => session.anchors_unavailable += 1,
    }
}

fn status_label(s: &AnchorStatus) -> &'static str {
    match s {
        AnchorStatus::Fresh => "Fresh",
        AnchorStatus::Moved => "Moved",
        AnchorStatus::Changed => "Changed",
        AnchorStatus::Orphaned => "Orphaned",
        AnchorStatus::MergeConflict => "MergeConflict",
        AnchorStatus::Submodule => "Submodule",
        AnchorStatus::ContentUnavailable(_) => "ContentUnavailable",
    }
}

fn mesh_is_reportable_in_stale_discovery(m: &MeshResolved) -> bool {
    m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh) || !m.pending.is_empty()
}

/// Resolve a small caller-provided list of mesh names without scanning all
/// mesh refs. Reuses one `EngineState` across the candidate set and resolves
/// each name through the live mesh ref. Preserves input order; per-name
/// resolution failures are returned alongside the name rather than aborting
/// the whole call so the path-index candidate workflow stays robust against a
/// stale path-index entry.
pub(crate) fn resolve_named_meshes(
    repo: &gix::Repository,
    names: &[String],
    options: EngineOptions,
) -> Result<Vec<(String, std::result::Result<MeshResolved, Error>)>> {
    let _perf = crate::perf::span("resolver.resolve-named-meshes");
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let resolved = resolve_mesh_with_state(repo, &mut state, name, options);
        out.push((name.clone(), resolved));
    }
    state.finish(repo);
    Ok(out)
}

fn stale_meshes_inner(
    repo: &gix::Repository,
    options: EngineOptions,
    enable_trace: bool,
) -> Result<(Vec<MeshResolved>, Vec<crate::perf::TraceRow>)> {
    crate::perf::reset_subroutine_counters();
    let mesh_refs = {
        let _perf = crate::perf::span("resolver.list-meshes");
        list_mesh_refs(repo)?
    };
    let mut out = Vec::new();
    let mut state = EngineState::new(repo, options.layers, options.needs_all_layers)?;
    if enable_trace {
        state.session.enable_trace();
    }
    {
        let _perf = crate::perf::span("resolver.resolve-stale-meshes");
        for (name, commit_oid) in mesh_refs {
            let mesh = {
                let _perf = crate::perf::span("resolver.read-mesh");
                read_mesh_from_commit(repo, &name, &commit_oid)?
            };
            // When tracing is active we must resolve every mesh so every anchor
            // gets a TraceRow. Skipping here would silently drop clean meshes from
            // the CSV and break the documented invariant `wc -l == anchors-total + 1`.
            if !enable_trace
                && can_skip_clean_head_pinned_mesh(repo, &mut state, &name, &mesh, options)?
            {
                continue;
            }
            let resolved = resolve_loaded_mesh_with_state(repo, &mut state, mesh, options)?;
            if mesh_is_reportable_in_stale_discovery(&resolved) {
                out.push(resolved);
            }
        }
    }
    crate::perf::counter("session.ensure-calls", state.session.ensure_calls);
    crate::perf::counter("session.ensure-hits", state.session.ensure_hits);
    crate::perf::counter("session.walks-len", state.session.walks_len() as u64);
    crate::perf::counter("session.interesting-commits", state.session.interesting_commits);
    crate::perf::counter("session.skipped-commits", state.session.skipped_commits);
    crate::perf::counter("session.pass1-ms", state.session.pass1_ms);
    crate::perf::counter("session.rename-trail-hits", state.session.rename_trail_hits);
    crate::perf::counter("session.rename-trail-misses", state.session.rename_trail_misses);
    crate::perf::counter("session.grouped-walk-exact-hits", state.session.grouped_walk_exact_hits);
    crate::perf::counter("session.grouped-walk-extend-hits", state.session.grouped_walk_extend_hits);
    crate::perf::counter("session.grouped-walk-misses", state.session.grouped_walk_misses);
    crate::perf::counter("session.name-status-hits", state.session.name_status_hits);
    crate::perf::counter("session.name-status-misses", state.session.name_status_misses);
    crate::perf::counter("session.blob-diff-hits", state.session.blob_diff_hits);
    crate::perf::counter("session.blob-diff-misses", state.session.blob_diff_misses);
    crate::perf::counter("session.drift-locus-hits", state.session.drift_locus_hits);
    crate::perf::counter("session.drift-locus-misses", state.session.drift_locus_misses);
    crate::perf::counter("session.filter-attr-hits", state.session.filter_attr_hits);
    crate::perf::counter("session.filter-attr-misses", state.session.filter_attr_misses);
    crate::perf::counter("session.cache-destructive-rebuilds", state.session.cache.destructive_rebuilds());
    // Category 1: hot-path subroutine counters. `filter-attr-*` come from
    // the engine-state memo (one increment per `filter_short_circuit` call,
    // misses count distinct paths); the remaining counters are process-global
    // and reset at the top of `stale_meshes`.
    crate::perf::counter(
        "session.filter-attr-calls",
        state.session.filter_attr_hits + state.session.filter_attr_misses,
    );
    crate::perf::counter(
        "session.filter-attr-distinct-paths",
        state.session.filter_attr_misses,
    );
    // Tier legend for the `session.*` family below:
    //   `gix-open-calls`     — count of `gix::open(...)` invocations the resolver
    //                          triggers (each pays `.git/config` parse + parent
    //                          walk; no internal caching).
    //   `attr-for-calls`     — count of [`crate::git::attr_for`] invocations.
    //                          gix's `Repository::index_or_load_from_head`
    //                          internally caches the `gix::index::File`, so the
    //                          actual `.git/index` open count (observable via
    //                          `strace -e openat -f -- ... | grep .git/index`)
    //                          is unrelated to this counter and typically far
    //                          smaller.
    //   `is-ancestor-*`      — out-of-process `git merge-base --is-ancestor`
    //                          subprocess invocations and in-process memo hits.
    crate::perf::counter("session.gix-open-calls", crate::perf::gix_open_calls());
    crate::perf::counter("session.attr-for-calls", crate::perf::attr_for_calls());
    crate::perf::counter(
        "session.is-ancestor-subprocess-calls",
        crate::perf::is_ancestor_subprocess_calls(),
    );
    crate::perf::counter(
        "session.is-ancestor-memo-hits",
        crate::perf::is_ancestor_memo_hits(),
    );
    // Category 2: anchor-set decomposition.
    let anchors_total = state.session.anchors_total();
    crate::perf::counter("session.anchors-total", anchors_total);
    crate::perf::counter("session.anchors-fresh", state.session.anchors_fresh);
    crate::perf::counter("session.anchors-moved", state.session.anchors_moved);
    crate::perf::counter("session.anchors-changed", state.session.anchors_changed);
    crate::perf::counter("session.anchors-orphaned", state.session.anchors_orphaned);
    crate::perf::counter(
        "session.anchors-merge-conflict",
        state.session.anchors_merge_conflict,
    );
    crate::perf::counter(
        "session.anchors-unavailable",
        state.session.anchors_unavailable,
    );
    crate::perf::counter(
        "session.anchors-fast-path-hits",
        state.session.anchors_fast_path_hits,
    );
    crate::perf::counter(
        "session.anchors-full-resolution",
        anchors_total.saturating_sub(state.session.anchors_fast_path_hits),
    );
    // Category 3: cache wall-clock, txn count, row counts, and per-anchor
    // resolution distribution.
    crate::perf::counter("cache.sqlite-read-ms", crate::perf::sqlite_read_ms());
    crate::perf::counter("cache.sqlite-write-ms", crate::perf::sqlite_write_ms());
    crate::perf::counter("cache.write-txn-count", crate::perf::write_txn_count());
    for (table, count) in state.session.cache.row_counts() {
        let label = match table {
            "name_status_cache" => "cache.name-status-rows",
            "blob_diff_cache" => "cache.blob-diff-rows",
            "grouped_walk_cache" => "cache.grouped-walk-rows",
            "rename_trail_cache" => "cache.rename-trail-rows",
            "drift_locus_cache" => "cache.drift-locus-rows",
            _ => continue,
        };
        crate::perf::counter(label, count);
    }
    {
        let mut per_anchor = std::mem::take(&mut state.session.per_anchor_us);
        per_anchor.sort_unstable();
        let percentile = |q: f64| -> u64 {
            if per_anchor.is_empty() {
                return 0;
            }
            let idx = ((per_anchor.len() as f64 - 1.0) * q).round() as usize;
            // Round to nearest millisecond for legibility.
            ((per_anchor[idx] + 500) / 1000) as u64
        };
        crate::perf::counter("resolve-anchor.p50-ms", percentile(0.50));
        crate::perf::counter("resolve-anchor.p95-ms", percentile(0.95));
    }
    // Legend: tiers are checked top-down (grouped-walk → rename-trail → name-status →
    // blob-diff → drift-locus). Zero hits+misses on a lower tier means a higher tier
    // absorbed all traffic (warm-run short-circuit).
    crate::perf::note("session.tier-legend: tiers checked top-down; zero counters on a lower tier indicate higher-tier short-circuiting");
    crate::perf::note(
        "session.group-legend: session.* counts in-process state and subroutine calls; \
         cache.* names sqlite-layer wall-clock and row counts; \
         resolve-anchor.* names per-anchor distribution",
    );
    let trace_rows = state.session.per_anchor_trace.take().unwrap_or_default();
    state.finish(repo);
    if out.len() > 1 {
        sort_meshes_by_anchor_path(&mut out);
    }
    Ok((out, trace_rows))
}

pub fn stale_meshes(repo: &gix::Repository, options: EngineOptions) -> Result<Vec<MeshResolved>> {
    let (meshes, _) = stale_meshes_inner(repo, options, false)?;
    Ok(meshes)
}

pub fn stale_meshes_with_trace(
    repo: &gix::Repository,
    options: EngineOptions,
) -> Result<(Vec<MeshResolved>, Vec<crate::perf::TraceRow>)> {
    stale_meshes_inner(repo, options, true)
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

pub(crate) fn sort_meshes_by_anchor_path(meshes: &mut [MeshResolved]) {
    let _perf = crate::perf::span("resolver.sort-meshes");
    if meshes.len() <= 1 {
        return;
    }

    // Build sort keys: sorted anchor paths per mesh
    let keys: Vec<Vec<PathBuf>> = meshes
        .iter()
        .map(|m| {
            let mut paths: Vec<PathBuf> =
                m.anchors.iter().map(|a| a.anchored.path.clone()).collect();
            paths.sort();
            paths
        })
        .collect();

    // Precompute overlap: does this mesh have extent overlap with any other
    // mesh that shares the exact same path tuple?
    let has_overlap: Vec<bool> = (0..meshes.len())
        .map(|i| {
            for j in 0..meshes.len() {
                if i != j && keys[i] == keys[j] && meshes_share_extent_overlap(&meshes[i], &meshes[j], &keys[i]) {
                    return true;
                }
            }
            false
        })
        .collect();

    // Sort indices by path tuple comparison + overlap sub-grouping
    let mut indices: Vec<usize> = (0..meshes.len()).collect();
    indices.sort_by(|&a, &b| {
        let paths_a = &keys[a];
        let paths_b = &keys[b];

        // Primary: lexicographic comparison of path tuples
        for (pa, pb) in paths_a.iter().zip(paths_b.iter()) {
            match pa.cmp(pb) {
                std::cmp::Ordering::Less => return std::cmp::Ordering::Less,
                std::cmp::Ordering::Greater => return std::cmp::Ordering::Greater,
                std::cmp::Ordering::Equal => continue,
            }
        }
        match paths_a.len().cmp(&paths_b.len()) {
            std::cmp::Ordering::Less => return std::cmp::Ordering::Less,
            std::cmp::Ordering::Greater => return std::cmp::Ordering::Greater,
            std::cmp::Ordering::Equal => {}
        }

        // Path tuples identical. Sub-group by extent overlap.
        match (has_overlap[a], has_overlap[b]) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    });

    // Apply permutation via cycle descent (convert from "from-permutation"
    // to "to-permutation" first)
    let mut perm: Vec<usize> = vec![0; meshes.len()];
    for (sorted_pos, &orig_pos) in indices.iter().enumerate() {
        perm[orig_pos] = sorted_pos;
    }
    for i in 0..meshes.len() {
        while perm[i] != i {
            let k = perm[i];
            meshes.swap(i, k);
            perm.swap(i, k);
        }
    }
}

fn extents_overlap(a: &AnchorExtent, b: &AnchorExtent) -> bool {
    match (a, b) {
        (AnchorExtent::WholeFile, _) | (_, AnchorExtent::WholeFile) => true,
        (
            AnchorExtent::LineRange {
                start: sa,
                end: ea,
            },
            AnchorExtent::LineRange {
                start: sb,
                end: eb,
            },
        ) => *sa <= *eb && *sb <= *ea,
    }
}

fn meshes_share_extent_overlap(
    a: &MeshResolved,
    b: &MeshResolved,
    paths: &[PathBuf],
) -> bool {
    for path in paths {
        for anc_a in &a.anchors {
            if anc_a.anchored.path != *path {
                continue;
            }
            for anc_b in &b.anchors {
                if anc_b.anchored.path != *path {
                    continue;
                }
                if extents_overlap(&anc_a.anchored.extent, &anc_b.anchored.extent) {
                    return true;
                }
            }
        }
    }
    false
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
    if mesh_has_staged_state(repo, name) {
        return Ok(false);
    }
    for (_, anchor) in &mesh.anchors_v2 {
        if anchor.anchor_sha != state.head_sha {
            return Ok(false);
        }
        if !anchor_path_is_layer_clean(state, &anchor.path) {
            return Ok(false);
        }
        if state.filter_short_circuit(repo, &anchor.path)?.is_some() {
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

/// Returns `true` when the workspace's enabled content layers agree
/// with HEAD *for `path` specifically*, even if some other path in the
/// workspace is dirty. The global `state.clean_layers` is a
/// fast-positive trivial-true shortcut so the genuinely-clean
/// workspace skips the per-path HashMap probes; the same shortcut
/// covers the "no content layers enabled" case.
pub(crate) fn anchor_path_is_layer_clean(state: &EngineState, path: &str) -> bool {
    if state.clean_layers || (!state.layers.index && !state.layers.worktree) {
        return true;
    }
    if state.conflicted_paths.contains(path) {
        return false;
    }
    if state.layers.index
        && state
            .index_diffs
            .as_ref()
            .is_some_and(|d| d.map.contains_key(path))
    {
        return false;
    }
    if state.layers.worktree
        && state
            .worktree_diffs
            .as_ref()
            .is_some_and(|d| d.map.contains_key(path))
    {
        return false;
    }
    true
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
        locus: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn make_mesh(name: &str, anchors: &[(&str, AnchorExtent)]) -> MeshResolved {
        MeshResolved {
            name: name.to_string(),
            message: String::new(),
            anchors: anchors
                .iter()
                .map(|(path, extent)| AnchorResolved {
                    anchor_id: String::new(),
                    anchor_sha: String::new(),
                    anchored: AnchorLocation {
                        path: PathBuf::from(path),
                        extent: *extent,
                        blob: None,
                    },
                    current: None,
                    status: AnchorStatus::Fresh,
                    source: None,
                    layer_sources: vec![],
                    acknowledged_by: None,
                    locus: None,
                })
                .collect(),
            pending: vec![],
        }
    }

    #[test]
    fn single_mesh_no_op() {
        let m = make_mesh("m1", &[]);
        let mut meshes = vec![m];
        sort_meshes_by_anchor_path(&mut meshes);
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].name, "m1");
    }

    #[test]
    fn primary_path_ordering() {
        let m1 = make_mesh("m1", &[("b.ts", AnchorExtent::WholeFile)]);
        let m2 = make_mesh("m2", &[("a.ts", AnchorExtent::WholeFile)]);
        let mut meshes = vec![m1, m2];
        sort_meshes_by_anchor_path(&mut meshes);
        assert_eq!(meshes[0].name, "m2");
        assert_eq!(meshes[1].name, "m1");
    }

    #[test]
    fn multi_path_tie_breaking() {
        let m1 = make_mesh(
            "m1",
            &[
                ("a.ts", AnchorExtent::WholeFile),
                ("c.ts", AnchorExtent::WholeFile),
            ],
        );
        let m2 = make_mesh(
            "m2",
            &[
                ("a.ts", AnchorExtent::WholeFile),
                ("b.ts", AnchorExtent::WholeFile),
            ],
        );
        let mut meshes = vec![m1, m2];
        sort_meshes_by_anchor_path(&mut meshes);
        assert_eq!(meshes[0].name, "m2");
        assert_eq!(meshes[1].name, "m1");
    }

    #[test]
    fn prefix_ordering() {
        let m1 = make_mesh("m1", &[("a.ts", AnchorExtent::WholeFile)]);
        let m2 = make_mesh(
            "m2",
            &[
                ("a.ts", AnchorExtent::WholeFile),
                ("b.ts", AnchorExtent::WholeFile),
            ],
        );
        let mut meshes = vec![m1, m2];
        sort_meshes_by_anchor_path(&mut meshes);
        assert_eq!(meshes[0].name, "m1");
        assert_eq!(meshes[1].name, "m2");
    }

    #[test]
    fn identical_paths_overlapping_extents() {
        let m1 = make_mesh(
            "m1",
            &[("a.ts", AnchorExtent::LineRange { start: 1, end: 10 })],
        );
        let m2 = make_mesh(
            "m2",
            &[("a.ts", AnchorExtent::LineRange { start: 5, end: 20 })],
        );
        let m3 = make_mesh(
            "m3",
            &[("a.ts", AnchorExtent::LineRange {
                start: 50,
                end: 100,
            })],
        );
        let mut meshes = vec![m1, m2, m3];
        sort_meshes_by_anchor_path(&mut meshes);
        // m1 and m2 overlap on a.ts, so they should be adjacent.
        // m3 has no overlap with either, so it sorts after the overlapping cluster.
        // Within the overlap cluster, stable sort preserves input order (m1 before m2).
        assert_eq!(meshes[0].name, "m1");
        assert_eq!(meshes[1].name, "m2");
        assert_eq!(meshes[2].name, "m3");
    }

    /// Regression: `EngineState::filter_short_circuit` memoizes per-path
    /// across an entire `stale` run. Two probes for the same path produce
    /// exactly one miss (the cold lookup); two probes for distinct paths
    /// produce two misses. This is the binding contract for the
    /// performance fix in main-65 — without the memo, every call to
    /// `filter_short_circuit` redid `gix::open` + `index_or_load_from_head`
    /// + `repo.attributes(…)`.
    #[test]
    fn filter_short_circuit_memoizes_per_path() {
        use std::process::Command;
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        for args in [
            &["init", "--initial-branch=main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ] {
            let out = Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        Command::new("git")
            .current_dir(dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        let out = Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
        assert!(out.status.success());

        let repo = gix::open(dir).unwrap();
        let mut state = EngineState::new(
            &repo,
            LayerSet { index: false, worktree: false, staged_mesh: false },
            true,
        )
        .unwrap();

        // First lookup for `a.txt` → miss.
        let _ = state.filter_short_circuit(&repo, "a.txt").unwrap();
        assert_eq!(state.session.filter_attr_misses, 1);
        assert_eq!(state.session.filter_attr_hits, 0);

        // Repeated lookup for the same path → hit, no new miss.
        let _ = state.filter_short_circuit(&repo, "a.txt").unwrap();
        let _ = state.filter_short_circuit(&repo, "a.txt").unwrap();
        assert_eq!(state.session.filter_attr_misses, 1);
        assert_eq!(state.session.filter_attr_hits, 2);

        // Distinct path → one additional miss.
        let _ = state.filter_short_circuit(&repo, "b.txt").unwrap();
        let _ = state.filter_short_circuit(&repo, "b.txt").unwrap();
        assert_eq!(state.session.filter_attr_misses, 2);
        assert_eq!(state.session.filter_attr_hits, 3);
    }

    fn state_for_predicate(
        layers: LayerSet,
        clean_layers: bool,
        index_paths: &[&str],
        worktree_paths: &[&str],
        conflicted: &[&str],
    ) -> EngineState {
        use crate::resolver::layers::LayerDiffs;
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        for args in [
            &["init", "--initial-branch=main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ] {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        std::fs::write(dir.join("seed"), "s\n").unwrap();
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let repo = gix::open(dir).unwrap();
        let mut state = EngineState::new(&repo, layers, true).unwrap();
        state.clean_layers = clean_layers;
        let mut idx = LayerDiffs::empty();
        for p in index_paths {
            idx.map.insert((*p).to_string(), crate::resolver::layers::diff::DiffEntry {
                new_path: (*p).to_string(),
                old_path: (*p).to_string(),
                hunks: vec![],
                new_blob: None,
                deleted: false,
                intent_to_add: false,
            });
        }
        state.index_diffs = Some(idx);
        let mut wt = LayerDiffs::empty();
        for p in worktree_paths {
            wt.map.insert((*p).to_string(), crate::resolver::layers::diff::DiffEntry {
                new_path: (*p).to_string(),
                old_path: (*p).to_string(),
                hunks: vec![],
                new_blob: None,
                deleted: false,
                intent_to_add: false,
            });
        }
        state.worktree_diffs = Some(wt);
        for p in conflicted {
            state.conflicted_paths.insert((*p).to_string());
        }
        state
    }

    #[test]
    fn anchor_path_predicate_clean_path() {
        let layers = LayerSet { index: true, worktree: true, staged_mesh: false };
        let state = state_for_predicate(layers, false, &["other.rs"], &["wiki/x.md"], &[]);
        assert!(anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_index_dirty() {
        let layers = LayerSet { index: true, worktree: true, staged_mesh: false };
        let state = state_for_predicate(layers, false, &["packages/anchor.rs"], &[], &[]);
        assert!(!anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_worktree_dirty() {
        let layers = LayerSet { index: true, worktree: true, staged_mesh: false };
        let state = state_for_predicate(layers, false, &[], &["packages/anchor.rs"], &[]);
        assert!(!anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_conflicted() {
        let layers = LayerSet { index: true, worktree: true, staged_mesh: false };
        let state = state_for_predicate(layers, false, &[], &[], &["packages/anchor.rs"]);
        assert!(!anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_layers_disabled() {
        let layers = LayerSet { index: false, worktree: false, staged_mesh: false };
        let state = state_for_predicate(layers, false, &[], &[], &["packages/anchor.rs"]);
        // With no content layers enabled, every path is trivially clean.
        assert!(anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_index_dirty_but_index_layer_off() {
        let layers = LayerSet { index: false, worktree: true, staged_mesh: false };
        let state = state_for_predicate(layers, false, &["packages/anchor.rs"], &[], &[]);
        // Index layer disabled → index diffs don't disqualify.
        assert!(anchor_path_is_layer_clean(&state, "packages/anchor.rs"));
    }

    #[test]
    fn anchor_path_predicate_clean_layers_shortcut() {
        let layers = LayerSet { index: true, worktree: true, staged_mesh: false };
        // clean_layers=true should trivially-true every path regardless
        // of conflicted_paths (which is logically empty under
        // clean_layers=true; the shortcut is what makes the genuinely
        // clean workspace skip the HashMap probes).
        let state = state_for_predicate(layers, true, &[], &[], &[]);
        assert!(anchor_path_is_layer_clean(&state, "anything.rs"));
    }

    #[test]
    fn determinism() {
        let m1 = make_mesh("m1", &[("b.ts", AnchorExtent::WholeFile)]);
        let m2 = make_mesh("m2", &[("a.ts", AnchorExtent::WholeFile)]);
        let m3 = make_mesh("m3", &[("c.ts", AnchorExtent::WholeFile)]);
        let mut meshes_a = vec![m1.clone(), m2.clone(), m3.clone()];
        let mut meshes_b = vec![m1, m2, m3];
        sort_meshes_by_anchor_path(&mut meshes_a);
        sort_meshes_by_anchor_path(&mut meshes_b);
        assert_eq!(meshes_a, meshes_b);
    }
}
