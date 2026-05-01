//! Auto-follow helper for `git mesh stale --auto-follow`.
//!
//! For each mesh with eligible `Moved` anchors (verbatim blob, same path, no
//! Changed sibling), `follow_moves` writes a single new mesh commit that
//! replaces the affected anchor entries and records the audit trail.

use crate::anchor::create_anchor_with_extent_skipping_blob_bounds;
use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::read::{read_mesh_at, serialize_config_blob};
use crate::mesh::path_index;
use std::collections::HashSet;
use crate::types::AnchorExtent;
use crate::{Error, Result};
use gix::objs::Tree;
use gix::objs::tree::{Entry, EntryKind};
use std::path::PathBuf;

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

/// A single resolved follow target: replace `anchor_id` in the mesh with
/// re-anchored `(new_path, new_extent)` at HEAD.
pub struct FollowDecision {
    pub anchor_id: String,
    pub new_path: PathBuf,
    pub new_extent: AnchorExtent,
}

/// Write a new mesh commit that replaces each anchor named in `decisions`
/// with its new location (re-anchored at HEAD).  The commit message is
/// inherited verbatim from the parent commit to preserve the user's authored
/// why — the audit trail is the new commit SHA, parent pointer, and reflog.
/// (Trade-off: no `mesh: follow N moved anchors` subject in the message, but
/// why-preservation takes priority over the audit string per spec.)
///
/// Uses the same CAS retry pattern as `commit_mesh` (MAX_RETRIES = 5).
pub fn follow_moves(
    repo: &gix::Repository,
    name: &str,
    decisions: &[FollowDecision],
) -> Result<String> {
    assert!(!decisions.is_empty(), "follow_moves called with no decisions");

    let wd = work_dir(repo)?;
    let mesh_ref_name = mesh_ref(name);
    let head_sha = git::head_oid(repo)?;

    const MAX_RETRIES: usize = 5;
    let mut attempt: usize = 0;

    let new_commit: String;
    'retry: loop {
        // Re-read mesh tip on every attempt so the CAS parent is fresh.
        let current_parent = resolve_ref_oid_optional(wd, &mesh_ref_name)?;
        let m = read_mesh_at(repo, name, current_parent.as_deref())?;

        // Build new anchor list: replace each matched id, keep all others.
        let mut combined: Vec<(String, crate::types::Anchor)> = Vec::new();
        'outer: for (id, anchor) in &m.anchors_v2 {
            for dec in decisions {
                if *id == dec.anchor_id {
                    // Re-anchor at HEAD with the new (path, extent).
                    let path_str = dec
                        .new_path
                        .to_str()
                        .ok_or_else(|| Error::Git("non-UTF-8 path in FollowDecision".into()))?;
                    let (new_id, new_anchor) = create_anchor_with_extent_skipping_blob_bounds(
                        repo,
                        &head_sha,
                        path_str,
                        dec.new_extent,
                    )?;
                    combined.push((new_id, new_anchor));
                    continue 'outer;
                }
            }
            combined.push((id.clone(), anchor.clone()));
        }

        // Sort by (path, extent) — same canonical order as commit_mesh.
        combined.sort_by(|a, b| {
            (a.1.path.as_str(), extent_sort_key(&a.1.extent))
                .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
        });

        // Guardrail: reject duplicate (path, extent) in the rebuilt list.
        // A concurrent staging at the same destination address can otherwise
        // produce duplicate anchors in the mesh tree.  Route through the
        // CAS-conflict retry branch.
        {
            let mut seen: HashSet<(String, (u32, u32))> = HashSet::new();
            for (_id, anchor) in &combined {
                let key = (anchor.path.clone(), extent_sort_key(&anchor.extent));
                if !seen.insert(key) {
                    attempt += 1;
                    if attempt >= MAX_RETRIES {
                        return Err(Error::ConcurrentUpdate {
                            expected: current_parent.unwrap_or_default(),
                            found: resolve_ref_oid_optional(wd, &mesh_ref_name)?
                                .unwrap_or_default(),
                        });
                    }
                    continue 'retry;
                }
            }
        }

        // Serialize anchors_v2.
        let anchors_v2_text: String = {
            let mut s = String::new();
            for (id, r) in &combined {
                s.push_str("id ");
                s.push_str(id);
                s.push('\n');
                s.push_str(&crate::anchor::serialize_anchor(r));
                s.push('\n');
            }
            s
        };
        let anchors_v2_blob = git::write_blob_bytes(repo, anchors_v2_text.as_bytes())?;

        // Config blob: preserve existing config unchanged.
        let config_text = serialize_config_blob(&m.config);
        let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;

        // Build tree.
        let tree = Tree {
            entries: vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: "anchors".into(),
                    oid: anchors_v2_blob
                        .parse()
                        .map_err(|e| Error::Git(format!("parse anchors blob oid: {e}")))?,
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

        // Commit message: structured audit subject on the subject line.
        // This makes `git log --oneline refs/meshes/v1/<mesh>` and
        // case-sensitive `git log --grep="^mesh: follow"` find the follow
        // commits.  `read_mesh_from_commit` walks past commits whose subject
        // starts with "mesh: follow " to recover the original user why.
        let n = decisions.len();
        let message = format!(
            "mesh: follow {} moved anchor{}\n",
            n,
            if n == 1 { "" } else { "s" }
        );

        let parents: Vec<String> = current_parent
            .as_deref()
            .map(|p| vec![p.to_string()])
            .unwrap_or_default();
        let candidate = create_commit(repo, &tree_oid, &message, &parents)?;

        // CAS ref transaction (mesh tip + path index).
        let old_anchors = &m.anchors_v2;
        let mesh_update = match current_parent.as_deref() {
            Some(prev) => RefUpdate::Update {
                name: mesh_ref_name.clone(),
                new_oid: candidate.clone(),
                expected_old_oid: prev.to_string(),
            },
            None => RefUpdate::Create {
                name: mesh_ref_name.clone(),
                new_oid: candidate.clone(),
            },
        };
        let mut updates =
            path_index::ref_updates_for_mesh(repo, name, old_anchors, &combined)?;
        updates.push(mesh_update);
        crate::git::ensure_log_all_ref_updates_always(repo)?;

        match apply_ref_transaction(wd, &updates) {
            Ok(()) => {
                new_commit = candidate;
                break;
            }
            Err(_e) => {
                attempt += 1;
                if attempt >= MAX_RETRIES {
                    return Err(Error::ConcurrentUpdate {
                        expected: current_parent.unwrap_or_default(),
                        found: resolve_ref_oid_optional(wd, &mesh_ref_name)?.unwrap_or_default(),
                    });
                }
                // Loop will re-read the fresh tip on the next iteration.
            }
        }
    }

    Ok(new_commit)
}

fn extent_sort_key(extent: &AnchorExtent) -> (u32, u32) {
    match *extent {
        AnchorExtent::WholeFile => (0, 0),
        AnchorExtent::LineRange { start, end } => (start, end),
    }
}
