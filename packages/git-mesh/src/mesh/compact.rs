//! Compaction engine — advances Fresh anchors to HEAD via CAS.
//!
//! This is the only write path added by `--compact`. Ordinary `git mesh stale`
//! never calls this module.

use crate::git::{self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir};
use crate::mesh::read::{read_mesh_at, serialize_config_blob};
use crate::resolver::resolve_mesh_at;
use crate::staging;
use crate::types::{AnchorExtent, AnchorStatus, EngineOptions};
use crate::{Error, Result};

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
        // Rewrite any Advanced records — those commits are orphans.
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
            advanced: 0, // invariant: conflicts > 0 => advanced == 0
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
    /// CAS retries exhausted; ref never updated — these commits are orphans.
    ConflictExhausted,
    /// AnchorStatus::Changed
    SkippedChanged,
    /// AnchorStatus::Orphaned
    SkippedOrphaned,
    /// AnchorStatus::MergeConflict
    SkippedMergeConflict,
    /// AnchorStatus::Submodule
    SkippedSubmodule,
    /// AnchorStatus::ContentUnavailable
    SkippedUnavailable,
    SkippedMoved,
    SkippedStagedOps,
    SkippedAlreadyHead,
}

// ---------------------------------------------------------------------------
// Core function.
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

    // 2. Read current mesh tip (CAS expected-old).
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;
    let mut current_tip = resolve_ref_oid_optional(wd, &mesh_ref)?
        .ok_or_else(|| Error::MeshNotFound(name.into()))?;

    const MAX_RETRIES: usize = 5;
    let mut attempt = 0;

    loop {
        // 3. Read mesh blob at the captured current_tip.
        let mesh = read_mesh_at(repo, name, Some(&current_tip))?;
        let head_sha = git::head_oid(repo)?; // re-read per attempt

        // 4. Resolve anchors HEAD-only, consistent with the captured current_tip.
        let resolved = resolve_mesh_at(repo, name, options, &current_tip)?;

        // 5. Classify each anchor.
        let mut compacted_anchors: Vec<(String, crate::types::Anchor)> = Vec::new();
        let mut unchanged_anchors: Vec<(String, crate::types::Anchor)> = Vec::new();
        let mut anchor_records: Vec<AnchorCompactRecord> = Vec::new();
        let mut advanced = 0u32;
        let mut skipped_stale = 0u32;
        let mut skipped_moved = 0u32;
        let mut skipped_clean_not_head = 0u32;

        for ar in &resolved.anchors {
            // Build old anchor view from mesh.anchors_v2 by anchor_id.
            // Graceful fallback if a concurrent writer removed the anchor.
            let old_anchor = mesh
                .anchors_v2
                .iter()
                .find(|(id, _)| id == &ar.anchor_id)
                .map(|(_, a)| a.clone());

            let Some(old_anchor) = old_anchor else {
                // Concurrent removal — skip.
                continue;
            };

            match ar.status {
                AnchorStatus::Fresh => {
                    if ar.anchor_sha == head_sha {
                        // Already at HEAD — idempotent no-op.
                        unchanged_anchors
                            .push((ar.anchor_id.clone(), old_anchor.clone()));
                        anchor_records.push(AnchorCompactRecord {
                            anchor_id: ar.anchor_id.clone(),
                            outcome: AnchorCompactOutcome::SkippedAlreadyHead,
                            old_commit: old_anchor.anchor_sha.clone(),
                            new_commit: None,
                            old_path: old_anchor.path.clone(),
                            new_path: None,
                            old_extent: old_anchor.extent,
                            new_extent: None,
                            old_blob: old_anchor.blob.clone(),
                            new_blob: None,
                        });
                        skipped_clean_not_head += 1;
                        continue;
                    }
                    // Rewrite: preserve anchor_id and created_at; advance to HEAD.
                    let current = ar.current.as_ref().expect("Fresh anchor must have current");
                    let path_str = current.path.to_string_lossy().into_owned();
                    let blob = git::path_blob_at(repo, &head_sha, &path_str)?;
                    let new_anchor = crate::types::Anchor {
                        anchor_sha: head_sha.clone(),
                        created_at: old_anchor.created_at.clone(), // preserved
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
                    compacted_anchors.push((ar.anchor_id.clone(), new_anchor));
                    advanced += 1;
                }
                AnchorStatus::Moved => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedMoved,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_moved += 1;
                }
                // Exhaustive — no `_` wildcard. A future AnchorStatus variant
                // causes a compile error, forcing an explicit decision.
                AnchorStatus::Changed => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedChanged,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_stale += 1;
                }
                AnchorStatus::Orphaned => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedOrphaned,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_stale += 1;
                }
                AnchorStatus::MergeConflict => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedMergeConflict,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_stale += 1;
                }
                AnchorStatus::Submodule => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedSubmodule,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_stale += 1;
                }
                AnchorStatus::ContentUnavailable(_) => {
                    unchanged_anchors.push((ar.anchor_id.clone(), old_anchor.clone()));
                    anchor_records.push(AnchorCompactRecord {
                        anchor_id: ar.anchor_id.clone(),
                        outcome: AnchorCompactOutcome::SkippedUnavailable,
                        old_commit: old_anchor.anchor_sha.clone(),
                        new_commit: None,
                        old_path: old_anchor.path.clone(),
                        new_path: None,
                        old_extent: old_anchor.extent,
                        new_extent: None,
                        old_blob: old_anchor.blob.clone(),
                        new_blob: None,
                    });
                    skipped_stale += 1;
                }
            }
        }

        if advanced == 0 {
            // F7: Even when no anchor advances, repair any path-index drift.
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
                let wd = work_dir(repo)?;
                crate::git::ensure_log_all_ref_updates_always(repo)?;
                // Best-effort: ignore errors (another writer may have already repaired).
                let _ = apply_ref_transaction(wd, &repair_updates);
            }
            return Ok(MeshCompactOutcome {
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
            });
        }

        // 6. Build new anchors.v2 (compacted ∪ unchanged), ordered by (path, extent).
        let mut all_anchors: Vec<(String, crate::types::Anchor)> =
            Vec::with_capacity(resolved.anchors.len());
        for ar in &resolved.anchors {
            if let Some(c) = compacted_anchors
                .iter()
                .find(|(cid, _)| cid == &ar.anchor_id)
            {
                all_anchors.push(c.clone());
            } else if let Some(u) = unchanged_anchors
                .iter()
                .find(|(uid, _)| uid == &ar.anchor_id)
            {
                all_anchors.push(u.clone());
            }
        }
        all_anchors.sort_by(|a, b| {
            (a.1.path.as_str(), extent_sort_key(&a.1.extent))
                .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
        });

        // 7. Build tree, create commit, CAS update.
        let config_text = serialize_config_blob(&mesh.config);
        let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;
        let anchors_v2_text = serialize_anchors_v2(&all_anchors);
        let anchors_v2_blob = git::write_blob_bytes(repo, anchors_v2_text.as_bytes())?;
        let tree_oid = build_mesh_tree(repo, &anchors_v2_blob, &config_blob)?;

        // Commit message: inherit why from parent mesh commit; strip any prior
        // git-mesh-compact: trailer before re-appending (idempotent).
        // Use rsplit_once so mid-text occurrences of the token are preserved.
        let why_body = mesh
            .message
            .trim()
            .rsplit_once("\n\ngit-mesh-compact:")
            .map(|(body, _)| body.trim())
            .unwrap_or_else(|| mesh.message.trim());
        let message = format!(
            "{}\n\ngit-mesh-compact: advanced {} anchor(s) to {}",
            why_body,
            advanced,
            &head_sha[..12],
        );
        let new_commit = create_commit(repo, &tree_oid, &message, &[current_tip.clone()])?;

        // Path-index update + mesh tip in one atomic ref transaction.
        let mut updates = super::path_index::ref_updates_for_mesh(
            repo,
            name,
            &mesh.anchors_v2,
            &all_anchors,
        )?;
        updates.push(RefUpdate::Update {
            name: mesh_ref.clone(),
            new_oid: new_commit.clone(),
            expected_old_oid: current_tip.clone(),
        });
        crate::git::ensure_log_all_ref_updates_always(repo)?;

        match apply_ref_transaction(wd, &updates) {
            Ok(()) => {
                return Ok(MeshCompactOutcome {
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
                });
            }
            Err(_) => {
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
                // Reread tip; loop will re-resolve and re-classify.
                current_tip = resolve_ref_oid_optional(wd, &mesh_ref)?
                    .ok_or_else(|| Error::MeshNotFound(name.into()))?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn staging_has_ops(s: &staging::Staging) -> bool {
    !s.adds.is_empty() || !s.removes.is_empty()
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
                filename: "anchors.v2".into(),
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

