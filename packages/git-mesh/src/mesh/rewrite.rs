//! Rewrite engine — advances anchor_sha values via CAS after a history rewrite.
//!
//! Called by `git mesh rewrite` (the `post-rewrite` hook handler). Reads a map
//! of old→new SHA pairs (git's post-rewrite protocol), and for each mesh
//! advances anchors whose `anchor_sha` matches an old SHA, provided the
//! anchored bytes are identical across the rewrite.

use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::read::{list_mesh_refs, read_mesh_from_commit, serialize_config_blob};
use crate::{Error, Result};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Public output types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RewriteOutcome {
    pub name: String,
    pub advanced: u32,
    pub skipped_blob_changed: u32,
    pub skipped_path_missing: u32,
    pub errors: u32,
    pub anchors: Vec<AnchorRewriteRecord>,
    pub hard_error: Option<String>,
}

impl RewriteOutcome {
    pub fn is_hard_error(&self) -> bool {
        self.hard_error.is_some()
    }

    fn error(name: &str, e: crate::Error) -> Self {
        Self {
            name: name.to_string(),
            advanced: 0,
            skipped_blob_changed: 0,
            skipped_path_missing: 0,
            errors: 1,
            anchors: Vec::new(),
            hard_error: Some(e.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnchorRewriteRecord {
    pub anchor_id: String,
    pub outcome: AnchorRewriteOutcome,
    pub old_sha: String,
    pub new_sha: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorRewriteOutcome {
    Advanced,
    SkippedBlobChanged,
    SkippedPathMissing,
    ConflictExhausted,
    NoMatch,
}

// ---------------------------------------------------------------------------
// Public entry point.
// ---------------------------------------------------------------------------

pub fn rewrite_meshes(
    repo: &gix::Repository,
    map: &HashMap<String, String>,
) -> Result<Vec<RewriteOutcome>> {
    if map.is_empty() {
        return Ok(Vec::new());
    }
    let refs = list_mesh_refs(repo)?;
    let mut outcomes = Vec::with_capacity(refs.len());
    for (name, _tip) in refs {
        let outcome =
            rewrite_one_mesh(repo, &name, map).unwrap_or_else(|e| RewriteOutcome::error(&name, e));
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

// ---------------------------------------------------------------------------
// Per-mesh CAS retry loop.
// ---------------------------------------------------------------------------

fn rewrite_one_mesh(
    repo: &gix::Repository,
    name: &str,
    map: &HashMap<String, String>,
) -> Result<RewriteOutcome> {
    const MAX_RETRIES: usize = 5;
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;

    let initial_tip =
        resolve_ref_oid_optional(wd, &mesh_ref)?.ok_or_else(|| Error::MeshNotFound(name.into()))?;

    let mut current_tip = initial_tip;
    let mut attempt = 0;

    loop {
        match apply_rewrite_attempt(repo, name, map, &current_tip)? {
            AttemptResult::Done(out) => return Ok(out),
            AttemptResult::CasConflict => {
                attempt += 1;
                if attempt >= MAX_RETRIES {
                    return Ok(RewriteOutcome {
                        name: name.to_string(),
                        advanced: 0,
                        skipped_blob_changed: 0,
                        skipped_path_missing: 0,
                        errors: 1,
                        anchors: Vec::new(),
                        hard_error: Some("CAS conflict exhausted retries".into()),
                    });
                }
                current_tip = resolve_ref_oid_optional(wd, &mesh_ref)?
                    .ok_or_else(|| Error::MeshNotFound(name.into()))?;
            }
        }
    }
}

enum AttemptResult {
    Done(RewriteOutcome),
    CasConflict,
}

fn apply_rewrite_attempt(
    repo: &gix::Repository,
    name: &str,
    map: &HashMap<String, String>,
    current_tip: &str,
) -> Result<AttemptResult> {
    let mesh = read_mesh_from_commit(repo, name, current_tip)?;
    let mesh_ref = format!("refs/meshes/v1/{name}");
    let wd = work_dir(repo)?;

    let mut anchor_records: Vec<AnchorRewriteRecord> = Vec::new();
    let mut new_anchors: Vec<(String, crate::types::Anchor)> = Vec::new();
    let mut advanced = 0u32;
    let mut skipped_blob_changed = 0u32;
    let mut skipped_path_missing = 0u32;

    for (id, anchor) in &mesh.anchors_v2 {
        let Some(new_sha) = map.get(&anchor.anchor_sha) else {
            // Not in map — carry over unchanged.
            new_anchors.push((id.clone(), anchor.clone()));
            continue;
        };

        // Try to read blob at old_sha and new_sha.
        let old_blob = git::path_blob_at(repo, &anchor.anchor_sha, &anchor.path);
        let new_blob = git::path_blob_at(repo, new_sha, &anchor.path);

        match (old_blob, new_blob) {
            (Ok(ob), Ok(nb)) => {
                if ob != nb {
                    // Blob changed — skip.
                    anchor_records.push(AnchorRewriteRecord {
                        anchor_id: id.clone(),
                        outcome: AnchorRewriteOutcome::SkippedBlobChanged,
                        old_sha: anchor.anchor_sha.clone(),
                        new_sha: Some(new_sha.clone()),
                        path: anchor.path.clone(),
                    });
                    skipped_blob_changed += 1;
                    new_anchors.push((id.clone(), anchor.clone()));
                } else {
                    // Advance.
                    let advanced_anchor = crate::types::Anchor {
                        anchor_sha: new_sha.clone(),
                        created_at: anchor.created_at.clone(),
                        path: anchor.path.clone(),
                        extent: anchor.extent,
                        blob: anchor.blob.clone(),
                    };
                    anchor_records.push(AnchorRewriteRecord {
                        anchor_id: id.clone(),
                        outcome: AnchorRewriteOutcome::Advanced,
                        old_sha: anchor.anchor_sha.clone(),
                        new_sha: Some(new_sha.clone()),
                        path: anchor.path.clone(),
                    });
                    advanced += 1;
                    new_anchors.push((id.clone(), advanced_anchor));
                }
            }
            _ => {
                // Path missing at either old or new.
                anchor_records.push(AnchorRewriteRecord {
                    anchor_id: id.clone(),
                    outcome: AnchorRewriteOutcome::SkippedPathMissing,
                    old_sha: anchor.anchor_sha.clone(),
                    new_sha: Some(new_sha.clone()),
                    path: anchor.path.clone(),
                });
                skipped_path_missing += 1;
                new_anchors.push((id.clone(), anchor.clone()));
            }
        }
    }

    if advanced == 0 {
        return Ok(AttemptResult::Done(RewriteOutcome {
            name: name.to_string(),
            advanced: 0,
            skipped_blob_changed,
            skipped_path_missing,
            errors: 0,
            anchors: anchor_records,
            hard_error: None,
        }));
    }

    // Sort by (path, extent) like compact does.
    new_anchors.sort_by(|a, b| {
        (a.1.path.as_str(), extent_sort_key(&a.1.extent))
            .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
    });

    // Build blob → tree → commit → CAS ref update.
    let config_text = serialize_config_blob(&mesh.config);
    let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;
    let anchors_v2_text = serialize_anchors_v2(&new_anchors);
    let anchors_v2_blob = git::write_blob_bytes(repo, anchors_v2_text.as_bytes())?;
    let tree_oid = build_mesh_tree(repo, &anchors_v2_blob, &config_blob)?;

    let message = mesh.message.trim().to_string();
    let new_commit = create_commit(repo, &tree_oid, &message, &[current_tip.to_string()])?;

    let update = RefUpdate::Update {
        name: mesh_ref.clone(),
        new_oid: new_commit.clone(),
        expected_old_oid: current_tip.to_string(),
    };
    crate::git::ensure_log_all_ref_updates_always(repo)?;

    match apply_ref_transaction(wd, &[update]) {
        Ok(()) => Ok(AttemptResult::Done(RewriteOutcome {
            name: name.to_string(),
            advanced,
            skipped_blob_changed,
            skipped_path_missing,
            errors: 0,
            anchors: anchor_records,
            hard_error: None,
        })),
        Err(_) => Ok(AttemptResult::CasConflict),
    }
}

// ---------------------------------------------------------------------------
// Private helpers (mirrors compact.rs private helpers).
// ---------------------------------------------------------------------------

fn extent_sort_key(extent: &crate::types::AnchorExtent) -> (u32, u32) {
    match *extent {
        crate::types::AnchorExtent::WholeFile => (0, 0),
        crate::types::AnchorExtent::LineRange { start, end } => (start, end),
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
