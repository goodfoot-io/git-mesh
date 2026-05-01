//! Compaction engine — advances Fresh anchors to HEAD via CAS.
//!
//! This is the only write path added by `--compact`. Ordinary `git mesh stale`
//! never calls this module.

use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::read::{read_mesh_at, read_mesh_from_commit, serialize_config_blob};
use crate::resolver::{
    EngineStateHandle, layers_filter_short_circuit, new_engine_state,
    resolve_loaded_mesh_with_engine_state, resolve_mesh_at, resolve_mesh_at_with_engine_state,
};
use crate::staging;
use crate::types::{AnchorExtent, AnchorStatus, EngineOptions, MeshResolved};
use crate::{Error, Result};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Public output types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MeshCompactOutcome {
    pub name: String,
    pub advanced: u32,
    pub skipped_stale: u32,
    pub skipped_moved: u32,
    pub skipped_clean_not_head: u32,
    pub skipped_staged: u32,
    pub conflicts: u32,
    pub errors: u32,
    pub anchors: Vec<AnchorCompactRecord>,
    pub hard_error: Option<String>,
    /// Set when whole mesh is skipped due to staged ops.
    pub staged_ops_present: bool,
}

impl MeshCompactOutcome {
    pub fn is_hard_error(&self) -> bool {
        self.hard_error.is_some()
    }

    pub fn error(name: &str, e: crate::Error) -> Self {
        Self {
            name: name.to_string(),
            advanced: 0,
            skipped_stale: 0,
            skipped_moved: 0,
            skipped_clean_not_head: 0,
            skipped_staged: 0,
            conflicts: 0,
            errors: 1,
            anchors: Vec::new(),
            hard_error: Some(e.to_string()),
            staged_ops_present: false,
        }
    }

    fn all_skipped_staged(name: &str) -> Self {
        Self {
            name: name.to_string(),
            advanced: 0,
            skipped_stale: 0,
            skipped_moved: 0,
            skipped_clean_not_head: 0,
            skipped_staged: 1,
            conflicts: 0,
            errors: 0,
            anchors: Vec::new(),
            hard_error: None,
            staged_ops_present: true,
        }
    }

    /// Returns a conflict outcome. Per the mutual-exclusion invariant,
    /// `advanced` is always 0 and any per-anchor Advanced records are
    /// rewritten to ConflictExhausted (the ref was never updated).
    fn conflict(
        name: &str,
        skipped_stale: u32,
        skipped_moved: u32,
        skipped_clean_not_head: u32,
        anchors: Vec<AnchorCompactRecord>,
    ) -> Self {
        let anchors = anchors
            .into_iter()
            .map(|mut a| {
                if a.outcome == AnchorCompactOutcome::Advanced {
                    a.outcome = AnchorCompactOutcome::ConflictExhausted;
                    a.new_commit = None;
                    a.new_path = None;
                    a.new_extent = None;
                    a.new_blob = None;
                }
                a
            })
            .collect();
        Self {
            name: name.to_string(),
            advanced: 0,
            skipped_stale,
            skipped_moved,
            skipped_clean_not_head,
            skipped_staged: 0,
            conflicts: 1,
            errors: 0,
            anchors,
            hard_error: None,
            staged_ops_present: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnchorCompactRecord {
    pub anchor_id: String,
    pub outcome: AnchorCompactOutcome,
    pub old_commit: String,
    pub new_commit: Option<String>,
    pub old_path: String,
    pub new_path: Option<String>,
    pub old_extent: AnchorExtent,
    pub new_extent: Option<AnchorExtent>,
    pub old_blob: String,
    pub new_blob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorCompactOutcome {
    Advanced,
    ConflictExhausted,
    SkippedChanged,
    SkippedOrphaned,
    SkippedMergeConflict,
    SkippedSubmodule,
    SkippedUnavailable,
    SkippedMoved,
    SkippedStagedOps,
    SkippedAlreadyHead,
}

// ---------------------------------------------------------------------------
// Already-at-HEAD fast path (Item 6).
// ---------------------------------------------------------------------------

/// Conservative no-op fast path. Returns the synthesized "all
/// SkippedAlreadyHead" outcome when every anchor in `mesh` already points
/// at HEAD, every anchor's path resolves to a HEAD blob equal to
/// `anchor.blob`, and no path triggers a filter short-circuit.
///
/// Mirrors `can_skip_clean_head_pinned_mesh` from the discovery side
/// (`stale_meshes`) but materializes per-anchor `SkippedAlreadyHead`
/// records so the JSON/human renderers see the same shape they do today
/// after a full resolution. Caller must verify there is no staged state.
fn already_at_head_outcome(
    repo: &gix::Repository,
    state: &mut EngineStateHandle,
    name: &str,
    mesh: &crate::types::Mesh,
) -> Result<Option<MeshCompactOutcome>> {
    let head_sha = state.head_sha().to_string();
    if mesh.anchors_v2.is_empty() {
        return Ok(None);
    }
    for (_, anchor) in &mesh.anchors_v2 {
        if anchor.anchor_sha != head_sha {
            return Ok(None);
        }
        if layers_filter_short_circuit(repo, &anchor.path)?.is_some() {
            return Ok(None);
        }
        let Some(head_blob) = state.head_blob_at(repo, &anchor.path)? else {
            return Ok(None);
        };
        if head_blob != anchor.blob {
            return Ok(None);
        }
    }
    let mut anchor_records = Vec::with_capacity(mesh.anchors_v2.len());
    let mut skipped_clean_not_head = 0u32;
    for (id, a) in &mesh.anchors_v2 {
        anchor_records.push(AnchorCompactRecord {
            anchor_id: id.clone(),
            outcome: AnchorCompactOutcome::SkippedAlreadyHead,
            old_commit: a.anchor_sha.clone(),
            new_commit: None,
            old_path: a.path.clone(),
            new_path: None,
            old_extent: a.extent,
            new_extent: None,
            old_blob: a.blob.clone(),
            new_blob: None,
        });
        skipped_clean_not_head += 1;
    }
    Ok(Some(MeshCompactOutcome {
        name: name.to_string(),
        advanced: 0,
        skipped_stale: 0,
        skipped_moved: 0,
        skipped_clean_not_head,
        skipped_staged: 0,
        conflicts: 0,
        errors: 0,
        anchors: anchor_records,
        hard_error: None,
        staged_ops_present: false,
    }))
}

// ---------------------------------------------------------------------------
// Single-mesh entry point.
// ---------------------------------------------------------------------------

pub fn compact_mesh(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
) -> Result<MeshCompactOutcome> {
    // 1. Check staging before any resolution.
    let staging = staging::read_staging(repo, name)?;
    if staging_has_ops(&staging) {
        return Ok(MeshCompactOutcome::all_skipped_staged(name));
    }

    // 2. Already-at-HEAD fast path (Item 6).
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;
    let initial_tip =
        resolve_ref_oid_optional(wd, &mesh_ref)?.ok_or_else(|| Error::MeshNotFound(name.into()))?;
    {
        let mesh = read_mesh_at(repo, name, Some(&initial_tip))?;
        let mut state = new_engine_state(repo, options)?;
        if let Some(out) = already_at_head_outcome(repo, &mut state, name, &mesh)? {
            return Ok(out);
        }
    }

    compact_mesh_with_retry(repo, name, options, initial_tip, None)
}

fn compact_mesh_with_retry(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
    initial_tip: String,
    mut shared_state: Option<&mut EngineStateHandle>,
) -> Result<MeshCompactOutcome> {
    const MAX_RETRIES: usize = 5;
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;
    let mut current_tip = initial_tip;
    let mut attempt = 0;

    loop {
        let mesh = read_mesh_at(repo, name, Some(&current_tip))?;
        // If the caller threaded a shared `EngineStateHandle` through
        // (the batch CAS-conflict path), reuse it as long as HEAD has
        // not moved since the state was built. The handle's HEAD-blob
        // cache is keyed on its captured `head_sha`, so reusing across
        // a HEAD movement would return stale blobs. On HEAD movement
        // we fall back to a fresh `resolve_mesh_at` for safety.
        let live_head = git::head_oid(repo)?;
        let resolved = match shared_state.as_deref_mut() {
            Some(handle) if handle.head_sha() == live_head.as_str() => {
                resolve_mesh_at_with_engine_state(repo, handle, name, options, &current_tip)?
            }
            _ => resolve_mesh_at(repo, name, options, &current_tip)?,
        };

        match apply_compact_attempt(repo, name, &mesh, &resolved, &current_tip)? {
            AttemptResult::Done(out) => return Ok(out),
            AttemptResult::CasConflict {
                skipped_stale,
                skipped_moved,
                skipped_clean_not_head,
                anchor_records,
            } => {
                attempt += 1;
                if attempt >= MAX_RETRIES {
                    return Ok(MeshCompactOutcome::conflict(
                        name,
                        skipped_stale,
                        skipped_moved,
                        skipped_clean_not_head,
                        anchor_records,
                    ));
                }
                current_tip = resolve_ref_oid_optional(wd, &mesh_ref)?
                    .ok_or_else(|| Error::MeshNotFound(name.into()))?;
            }
        }
    }
}

enum AttemptResult {
    Done(MeshCompactOutcome),
    CasConflict {
        skipped_stale: u32,
        skipped_moved: u32,
        skipped_clean_not_head: u32,
        anchor_records: Vec<AnchorCompactRecord>,
    },
}

/// Run one classification + CAS attempt for a mesh. Caller owns retry.
///
/// Item 8: rebuild `all_anchors` via HashMap lookups keyed by anchor_id
/// rather than scanning `mesh.anchors_v2`, `compacted`, and `unchanged`
/// linearly per anchor.
fn apply_compact_attempt(
    repo: &gix::Repository,
    name: &str,
    mesh: &crate::types::Mesh,
    resolved: &MeshResolved,
    current_tip: &str,
) -> Result<AttemptResult> {
    let head_sha = git::head_oid(repo)?;
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;

    // Item 8: anchor_id -> &Anchor map for prior mesh state.
    let old_by_id: HashMap<&str, &crate::types::Anchor> = mesh
        .anchors_v2
        .iter()
        .map(|(id, a)| (id.as_str(), a))
        .collect();

    let mut compacted_by_id: HashMap<String, crate::types::Anchor> = HashMap::new();
    let mut unchanged_by_id: HashMap<String, crate::types::Anchor> = HashMap::new();
    let mut anchor_records: Vec<AnchorCompactRecord> = Vec::with_capacity(resolved.anchors.len());
    let mut advanced_old_anchor_shas: Vec<String> = Vec::new();
    let mut advanced = 0u32;
    let mut skipped_stale = 0u32;
    let mut skipped_moved = 0u32;
    let mut skipped_clean_not_head = 0u32;

    for ar in &resolved.anchors {
        let Some(old_anchor) = old_by_id.get(ar.anchor_id.as_str()).map(|a| (*a).clone()) else {
            continue;
        };

        match ar.status {
            AnchorStatus::Fresh => {
                if ar.anchor_sha == head_sha {
                    unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                    anchor_records.push(skipped_record(
                        &ar.anchor_id,
                        AnchorCompactOutcome::SkippedAlreadyHead,
                        &old_anchor,
                    ));
                    skipped_clean_not_head += 1;
                    continue;
                }
                let current = ar.current.as_ref().expect("Fresh anchor must have current");
                let path_str = current.path.to_string_lossy().into_owned();
                let blob = git::path_blob_at(repo, &head_sha, &path_str)?;
                let new_anchor = crate::types::Anchor {
                    anchor_sha: head_sha.clone(),
                    created_at: old_anchor.created_at.clone(),
                    path: path_str.clone(),
                    extent: current.extent,
                    blob: blob.clone(),
                };
                anchor_records.push(AnchorCompactRecord {
                    anchor_id: ar.anchor_id.clone(),
                    outcome: AnchorCompactOutcome::Advanced,
                    old_commit: old_anchor.anchor_sha.clone(),
                    new_commit: Some(head_sha.clone()),
                    old_path: old_anchor.path.clone(),
                    new_path: Some(path_str),
                    old_extent: old_anchor.extent,
                    new_extent: Some(current.extent),
                    old_blob: old_anchor.blob.clone(),
                    new_blob: Some(blob),
                });
                advanced_old_anchor_shas.push(old_anchor.anchor_sha.clone());
                compacted_by_id.insert(ar.anchor_id.clone(), new_anchor);
                advanced += 1;
            }
            AnchorStatus::Moved => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedMoved,
                    &old_anchor,
                ));
                skipped_moved += 1;
            }
            AnchorStatus::Changed => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedChanged,
                    &old_anchor,
                ));
                skipped_stale += 1;
            }
            AnchorStatus::Orphaned => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedOrphaned,
                    &old_anchor,
                ));
                skipped_stale += 1;
            }
            AnchorStatus::MergeConflict => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedMergeConflict,
                    &old_anchor,
                ));
                skipped_stale += 1;
            }
            AnchorStatus::Submodule => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedSubmodule,
                    &old_anchor,
                ));
                skipped_stale += 1;
            }
            AnchorStatus::ContentUnavailable(_) => {
                unchanged_by_id.insert(ar.anchor_id.clone(), old_anchor.clone());
                anchor_records.push(skipped_record(
                    &ar.anchor_id,
                    AnchorCompactOutcome::SkippedUnavailable,
                    &old_anchor,
                ));
                skipped_stale += 1;
            }
        }
    }

    if advanced == 0 {
        let drift_updates = super::path_index::ref_updates_for_mesh(
            repo,
            name,
            &mesh.anchors_v2,
            &mesh.anchors_v2,
        )?;
        let repair_updates: Vec<RefUpdate> = drift_updates
            .into_iter()
            .filter(|u| matches!(u, RefUpdate::Update { new_oid, expected_old_oid, .. } if new_oid != expected_old_oid))
            .collect();
        if !repair_updates.is_empty() {
            crate::git::ensure_log_all_ref_updates_always(repo)?;
            let _ = apply_ref_transaction(wd, &repair_updates);
        }
        return Ok(AttemptResult::Done(MeshCompactOutcome {
            name: name.to_string(),
            advanced: 0,
            skipped_stale,
            skipped_moved,
            skipped_clean_not_head,
            skipped_staged: 0,
            conflicts: 0,
            errors: 0,
            anchors: anchor_records,
            hard_error: None,
            staged_ops_present: false,
        }));
    }

    // Item 8: rebuild new anchors blob via HashMap lookups in resolved order.
    let mut all_anchors: Vec<(String, crate::types::Anchor)> =
        Vec::with_capacity(resolved.anchors.len());
    for ar in &resolved.anchors {
        if let Some(c) = compacted_by_id.get(&ar.anchor_id) {
            all_anchors.push((ar.anchor_id.clone(), c.clone()));
        } else if let Some(u) = unchanged_by_id.get(&ar.anchor_id) {
            all_anchors.push((ar.anchor_id.clone(), u.clone()));
        }
    }
    all_anchors.sort_by(|a, b| {
        (a.1.path.as_str(), extent_sort_key(&a.1.extent))
            .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
    });

    let config_text = serialize_config_blob(&mesh.config);
    let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;
    let anchors_v2_text = serialize_anchors_v2(&all_anchors);
    let anchors_v2_blob = git::write_blob_bytes(repo, anchors_v2_text.as_bytes())?;
    let tree_oid = build_mesh_tree(repo, &anchors_v2_blob, &config_blob)?;

    let message = mesh.message.trim().to_string();
    let new_commit = create_commit(repo, &tree_oid, &message, &[current_tip.to_string()])?;

    let mut updates =
        super::path_index::ref_updates_for_mesh(repo, name, &mesh.anchors_v2, &all_anchors)?;
    updates.push(RefUpdate::Update {
        name: mesh_ref.clone(),
        new_oid: new_commit.clone(),
        expected_old_oid: current_tip.to_string(),
    });
    crate::git::ensure_log_all_ref_updates_always(repo)?;

    match apply_ref_transaction(wd, &updates) {
        Ok(()) => {
            for sha in &advanced_old_anchor_shas {
                if let Err(e) = crate::resolver::trail_cache::clear(repo, sha) {
                    eprintln!("git-mesh: trail_cache::clear failed for {sha}: {e}");
                }
            }
            Ok(AttemptResult::Done(MeshCompactOutcome {
                name: name.to_string(),
                advanced,
                skipped_stale,
                skipped_moved,
                skipped_clean_not_head,
                skipped_staged: 0,
                conflicts: 0,
                errors: 0,
                anchors: anchor_records,
                hard_error: None,
                staged_ops_present: false,
            }))
        }
        Err(_) => Ok(AttemptResult::CasConflict {
            skipped_stale,
            skipped_moved,
            skipped_clean_not_head,
            anchor_records,
        }),
    }
}

// ---------------------------------------------------------------------------
// Batch entry point (Item 5).
// ---------------------------------------------------------------------------

/// Compact every named mesh using a single shared `EngineState` for the
/// initial pass. On CAS conflict for a given mesh, fall back to the
/// single-mesh path which retries that mesh in isolation with a fresh
/// `EngineState`. Each outcome is yielded to `on_outcome` as the mesh
/// finishes so `--format=json` can stream NDJSON.
pub fn compact_meshes_batch<F>(
    repo: &gix::Repository,
    names: &[String],
    options: EngineOptions,
    mut on_outcome: F,
) -> Result<Vec<MeshCompactOutcome>>
where
    F: FnMut(&MeshCompactOutcome) -> Result<()>,
{
    let _perf = crate::perf::span("compact.batch");
    let wd = work_dir(repo)?;
    let mut state = new_engine_state(repo, options)?;
    let mut outcomes = Vec::with_capacity(names.len());

    for name in names {
        let outcome = compact_one_in_batch(repo, wd, &mut state, name, options)
            .unwrap_or_else(|e| MeshCompactOutcome::error(name, e));
        on_outcome(&outcome)?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

fn compact_one_in_batch(
    repo: &gix::Repository,
    wd: &std::path::Path,
    state: &mut EngineStateHandle,
    name: &str,
    options: EngineOptions,
) -> Result<MeshCompactOutcome> {
    // Staging gate.
    let staging = staging::read_staging(repo, name)?;
    if staging_has_ops(&staging) {
        return Ok(MeshCompactOutcome::all_skipped_staged(name));
    }

    let mesh_ref = format!("refs/meshes/v1/{name}");
    let initial_tip =
        resolve_ref_oid_optional(wd, &mesh_ref)?.ok_or_else(|| Error::MeshNotFound(name.into()))?;
    let mesh = read_mesh_from_commit(repo, name, &initial_tip)?;

    // Already-at-HEAD fast path using the shared state.
    if let Some(out) = already_at_head_outcome(repo, state, name, &mesh)? {
        return Ok(out);
    }

    // Resolve through the shared state, then run a single attempt.
    let resolved = resolve_loaded_mesh_with_engine_state(repo, state, mesh.clone(), options)?;
    match apply_compact_attempt(repo, name, &mesh, &resolved, &initial_tip)? {
        AttemptResult::Done(out) => Ok(out),
        AttemptResult::CasConflict { .. } => {
            // CAS conflict: fall back to the single-mesh retry loop with
            // a fresh state localized to this mesh. Re-read the tip so
            // the retry classifies against the latest blob.
            let fresh_tip = resolve_ref_oid_optional(wd, &mesh_ref)?
                .ok_or_else(|| Error::MeshNotFound(name.into()))?;
            compact_mesh_with_retry(repo, name, options, fresh_tip, Some(state))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn staging_has_ops(s: &staging::Staging) -> bool {
    !s.adds.is_empty() || !s.removes.is_empty() || !s.configs.is_empty() || s.why.is_some()
}

fn skipped_record(
    anchor_id: &str,
    outcome: AnchorCompactOutcome,
    old: &crate::types::Anchor,
) -> AnchorCompactRecord {
    AnchorCompactRecord {
        anchor_id: anchor_id.to_string(),
        outcome,
        old_commit: old.anchor_sha.clone(),
        new_commit: None,
        old_path: old.path.clone(),
        new_path: None,
        old_extent: old.extent,
        new_extent: None,
        old_blob: old.blob.clone(),
        new_blob: None,
    }
}

fn extent_sort_key(extent: &AnchorExtent) -> (u32, u32) {
    match *extent {
        AnchorExtent::WholeFile => (0, 0),
        AnchorExtent::LineRange { start, end } => (start, end),
    }
}

fn serialize_anchors_v2(anchors: &[(String, crate::types::Anchor)]) -> String {
    let mut s = String::new();
    for (id, r) in anchors {
        s.push_str("id ");
        s.push_str(id);
        s.push('\n');
        s.push_str(&crate::anchor::serialize_anchor(r));
        s.push('\n');
    }
    s
}

fn build_mesh_tree(
    repo: &gix::Repository,
    anchors_v2_blob: &str,
    config_blob: &str,
) -> Result<String> {
    use gix::objs::Tree;
    use gix::objs::tree::{Entry, EntryKind};
    let tree = Tree {
        entries: vec![
            Entry {
                mode: EntryKind::Blob.into(),
                filename: "anchors".into(),
                oid: anchors_v2_blob
                    .parse()
                    .map_err(|e| Error::Git(format!("parse anchors_v2 blob oid: {e}")))?,
            },
            Entry {
                mode: EntryKind::Blob.into(),
                filename: "config".into(),
                oid: config_blob
                    .parse()
                    .map_err(|e| Error::Git(format!("parse config blob oid: {e}")))?,
            },
        ],
    };
    let tree_oid = repo
        .write_object(&tree)
        .map_err(|e| Error::Git(format!("write tree: {e}")))?
        .detach()
        .to_string();
    Ok(tree_oid)
}
