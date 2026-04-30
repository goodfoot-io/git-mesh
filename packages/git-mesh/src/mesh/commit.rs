//! Mesh commit pipeline — §6.1, §6.2.

use crate::anchor::create_anchor_with_extent_skipping_blob_bounds;
use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::read::{parse_config_blob, serialize_config_blob};
use crate::staging::{self, StagedConfig, Staging};
use crate::types::{AnchorExtent, Mesh, MeshConfig};
use crate::validation::validate_mesh_name;
use crate::{Error, Result};
use gix::objs::Tree;
use gix::objs::tree::{Entry, EntryKind};
use std::path::Path;

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

pub fn commit_mesh(repo: &gix::Repository, name: &str) -> Result<String> {
    validate_mesh_name(name)?;
    let wd = work_dir(repo)?;
    let staging = staging::read_staging(repo, name)?;

    let mesh_ref = mesh_ref(name);
    let base_tip = resolve_ref_oid_optional(wd, &mesh_ref)?;

    // Load current state (if any).
    let (anchor_v2_records, base_config, base_message) = match base_tip.as_deref() {
        Some(tip) => {
            let m = super::read::read_mesh_at(repo, name, Some(tip))?;
            (m.anchors_v2, m.config, Some(m.message))
        }
        None => (
            Vec::new(),
            MeshConfig {
                copy_detection: crate::types::DEFAULT_COPY_DETECTION,
                ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
            },
            None,
        ),
    };

    // Dedup adds by `(path, extent)` last-write-wins (plan §D5). The
    // staging walk yields adds in append order; the *last* occurrence
    // wins because its sidecar bytes are the most recent capture.
    let staging = dedup_staged_adds(staging);

    // Validate removes exist and adds don't collide post-remove. Work on a
    // materialized snapshot `(anchor_id, path, extent)`.
    let mut snapshots: Vec<(String, crate::types::Anchor)> = anchor_v2_records.clone();
    for rem in &staging.removes {
        let idx = snapshots
            .iter()
            .position(|(_, a)| a.path == rem.path && a.extent == rem.extent)
            .ok_or_else(|| Error::AnchorNotInMesh {
                path: rem.path.clone(),
                start: rem.start(),
                end: rem.end(),
            })?;
        snapshots.remove(idx);
    }
    // A staged add can carry the anchor_id it supersedes in its sidecar
    // metadata. This is what makes re-anchoring a moved anchor work: the
    // new address may not match the old `(path, extent)`, but it still
    // replaces the same durable anchor relationship.
    for a in &staging.adds {
        let Some(meta) = staging::read_sidecar_meta(repo, name, a.line_number) else {
            continue;
        };
        let Some(anchor_id) = meta.anchor_id else {
            continue;
        };
        if let Some(idx) = snapshots.iter().position(|(id, _)| id == &anchor_id) {
            snapshots.remove(idx);
        }
    }
    // Adds that collide with an existing anchor at `(path, extent)` are
    // dedup-overrides per §D5: drop the prior snapshot, keep the staged
    // add.
    for a in &staging.adds {
        if let Some(idx) = snapshots
            .iter()
            .position(|(_, a_old)| a_old.path == a.path && a_old.extent == a.extent)
        {
            snapshots.remove(idx);
        }
    }

    // Resolve final config: baseline <- staged (last-write-wins).
    let mut new_config = base_config;
    let (new_cd, new_iw) = staging::resolve_staged_config(
        &staging,
        (base_config.copy_detection, base_config.ignore_whitespace),
    );
    new_config.copy_detection = new_cd;
    new_config.ignore_whitespace = new_iw;

    let config_changed = new_config != base_config;
    let meaningful_adds = !staging.adds.is_empty();
    let meaningful_removes = !staging.removes.is_empty();
    let meaningful_why = staging.why.is_some();

    if !meaningful_adds && !meaningful_removes && !config_changed && !meaningful_why {
        if staging.configs.is_empty() && staging.adds.is_empty() && staging.removes.is_empty() {
            return Err(Error::StagingEmpty(name.into()));
        }
        // Only staged configs, none changed value: ConfigNoOp.
        if let Some(first) = staging.configs.first() {
            let (key, value) = match first {
                StagedConfig::CopyDetection(cd) => (
                    "copy-detection",
                    staging::serialize_copy_detection(*cd).to_string(),
                ),
                StagedConfig::IgnoreWhitespace(b) => ("ignore-whitespace", b.to_string()),
            };
            return Err(Error::ConfigNoOp {
                key: key.into(),
                value,
            });
        }
        return Err(Error::StagingEmpty(name.into()));
    }

    // Determine the git commit message: staged why wins, else inherit
    // the parent mesh commit's message, else hard-fail with `WhyRequired`.
    // The `message` variable below is git-layer vocabulary — it is the
    // bytes handed to `gix::commit` as the commit message.
    let message = match (&staging.why, &base_message) {
        (Some(m), _) => m.clone(),
        (None, Some(prior)) => prior.clone(),
        (None, None) => return Err(Error::WhyRequired(name.into())),
    };

    // Slice 4: hard-fail on any tampered sidecar BEFORE any anchor refs
    // are written. `<fail-closed>`: a missing/unreadable meta or an
    // empty/non-matching `content_sha256` is treated as tampered.
    for a in &staging.adds {
        match staging::read_sidecar_verified(repo, name, a.line_number) {
            Ok(_) => {}
            Err(staging::SidecarVerifyError::Tampered) => {
                return Err(Error::SidecarTampered {
                    mesh: name.to_string(),
                    index: a.line_number,
                });
            }
            // Missing sidecar bytes are a separate corruption class
            // already reported by `doctor` / surfaced downstream by the
            // sidecar-meta lookup below; let the line-count read raise
            // its own error rather than masking it here.
            Err(staging::SidecarVerifyError::Missing) => {}
        }
    }

    // Drift check and anchor creation for staged adds. All-or-nothing:
    // create anchor refs for each add; on any failure propagate.
    let head_sha = git::head_oid(repo)?;
    let mut new_anchors: Vec<(String, crate::types::Anchor)> = Vec::new();
    // Pre-validate every add against its resolved anchor (prevent partial
    // writes) BEFORE creating any anchor refs.
    for a in &staging.adds {
        let anchor = a.anchor.clone().unwrap_or_else(|| head_sha.clone());
        match a.extent {
            AnchorExtent::LineRange { start, end } => {
                // Slice 1: source the line count from the sidecar meta
                // (captured at stage-time from filtered worktree bytes),
                // *never* from the raw blob — the latter is the LFS
                // pointer for `filter=lfs` paths and would always trip.
                // `append_prepared_add` writes the meta unconditionally,
                // so a missing file here is a corrupted staging area;
                // fail closed (CLAUDE.md `<fail-closed>`) instead of
                // re-rendering, which would diverge from the bytes the
                // resolver later reads from the same sidecar.
                let line_count = staging::read_sidecar_meta(repo, name, a.line_number)
                    .map(|m| m.line_count)
                    .ok_or_else(|| Error::Git(format!(
                        "missing or unreadable sidecar meta for staged add `{}` (mesh `{}`, slot {})",
                        a.path, name, a.line_number
                    )))?;
                if start < 1 || end < start || end > line_count {
                    return Err(Error::InvalidAnchor { start, end });
                }
            }
            AnchorExtent::WholeFile => {
                // Confirm the path resolves to a tree entry; gitlink
                // and blob both acceptable.
                if crate::git::path_blob_at(repo, &anchor, &a.path).is_err()
                    && !path_exists_in_tree(repo, &anchor, &a.path)
                {
                    return Err(Error::PathNotInTree {
                        path: a.path.clone(),
                        commit: anchor.clone(),
                    });
                }
            }
        }
    }
    for a in &staging.adds {
        let anchor = a.anchor.clone().unwrap_or_else(|| head_sha.clone());
        let (id, anchor_rec) =
            create_anchor_with_extent_skipping_blob_bounds(repo, &anchor, &a.path, a.extent)?;
        new_anchors.push((id, anchor_rec));
    }

    // CAS retry loop (§6). Anchor blobs are content-addressed and already
    // written; only the tree/commit/ref-update step needs retrying. On
    // conflict, reload the mesh tip, re-validate post-remove collisions
    // against the new snapshot, rebuild the tree/commit with the new
    // parent, and retry.
    let mut current_parent = base_tip.clone();
    let mut current_tip_anchors = anchor_v2_records;
    let mut current_snapshots = snapshots;
    const MAX_RETRIES: usize = 5;
    let new_commit: String;
    let mut attempt: usize = 0;
    loop {
        // Combine anchors and canonicalize by (path, extent).
        let mut combined: Vec<(String, crate::types::Anchor)> = current_snapshots.clone();
        for (id, r) in &new_anchors {
            combined.push((id.clone(), r.clone()));
        }
        combined.sort_by(|a, b| {
            (a.1.path.as_str(), extent_sort_key(&a.1.extent))
                .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
        });

        // Build tree: `anchors.v2` blob + `config` blob.
        let config_text = serialize_config_blob(&new_config);
        let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;

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

        // Build a tree with `anchors.v2` and `config` entries.
        let tree = Tree {
            entries: vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: "anchors.v2".into(),
                    oid: anchors_v2_blob.parse().map_err(|e| {
                        crate::Error::Git(format!("parse anchors_v2 blob oid: {e}"))
                    })?,
                },
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: "config".into(),
                    oid: config_blob
                        .parse()
                        .map_err(|e| crate::Error::Git(format!("parse config blob oid: {e}")))?,
                },
            ],
        };
        let tree_oid = repo
            .write_object(&tree)
            .map_err(|e| crate::Error::Git(format!("write tree: {e}")))?
            .detach()
            .to_string();

        // Commit.
        let parents: Vec<String> = current_parent
            .as_deref()
            .map(|p| vec![p.to_string()])
            .unwrap_or_default();
        let candidate = create_commit(repo, &tree_oid, &message, &parents)?;

        // Atomic CAS update. The authoritative path index is updated in
        // the same ref transaction as the mesh tip, so a successful mesh
        // commit cannot leave filtered reads with stale candidates.
        let mesh_update = match current_parent.as_deref() {
            Some(prev) => RefUpdate::Update {
                name: mesh_ref.clone(),
                new_oid: candidate.clone(),
                expected_old_oid: prev.to_string(),
            },
            None => RefUpdate::Create {
                name: mesh_ref.clone(),
                new_oid: candidate.clone(),
            },
        };
        let mut updates =
            super::path_index::ref_updates_for_mesh(repo, name, &current_tip_anchors, &combined)?;
        updates.push(mesh_update);
        // Slice 6d: lazily ensure `core.logAllRefUpdates = always` so
        // refs under `refs/meshes/*` get reflog entries. Doctor reports
        // an INFO finding when it would set this.
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
                        expected: current_parent.clone().unwrap_or_default(),
                        found: resolve_ref_oid_optional(wd, &mesh_ref)?.unwrap_or_default(),
                    });
                }
                // Re-read the mesh tip and path-index refs before retrying.
                // The transaction can fail on either the mesh CAS or one of
                // the affected path buckets.
                let latest = resolve_ref_oid_optional(wd, &mesh_ref)?;
                current_parent = latest;
                // Re-materialize snapshot from new tip and re-run
                // post-remove / add collision validation.
                let new_snapshots = match current_parent.as_deref() {
                    Some(tip) => {
                        let m = super::read::read_mesh_at(repo, name, Some(tip))?;
                        m.anchors_v2
                    }
                    None => Vec::new(),
                };
                current_tip_anchors = new_snapshots.clone();
                let mut post = new_snapshots;
                for rem in &staging.removes {
                    let idx = post
                        .iter()
                        .position(|(_, a)| a.path == rem.path && a.extent == rem.extent)
                        .ok_or_else(|| Error::AnchorNotInMesh {
                            path: rem.path.clone(),
                            start: rem.start(),
                            end: rem.end(),
                        })?;
                    post.remove(idx);
                }
                // Adds at the same `(path, extent)` as an existing anchor
                // are treated as last-write-wins overrides (plan §D5).
                for a in &staging.adds {
                    if let Some(idx) = post
                        .iter()
                        .position(|(_, a_old)| a_old.path == a.path && a_old.extent == a.extent)
                    {
                        post.remove(idx);
                    }
                }
                current_snapshots = post;
            }
        }
    }

    // Clear staging on success.
    let _ = staging::clear_staging(repo, name);

    Ok(new_commit)
}

// Silence unused-import warnings when the above is refactored.
#[allow(dead_code)]
fn _unused(_: &Mesh, _: &Path, _: &Staging, _: fn(&str) -> Result<MeshConfig>) {
    let _ = parse_config_blob;
}

/// Last-write-wins dedup of `staging.adds` by `(path, extent)`. The
/// staging walk yields adds in append order with line numbers `1..N`
/// matching the on-disk `<mesh>.<N>` sidecar suffix; we keep the
/// highest `line_number` per key (plan §D5: order by `N` descending,
/// ties broken by mtime then suffix — which here reduces to "later
/// write wins" since the parser already orders by file position).
fn dedup_staged_adds(mut staging: Staging) -> Staging {
    use std::collections::HashMap;
    let mut last_for_key: HashMap<(String, AnchorExtent), u32> = HashMap::new();
    for a in &staging.adds {
        let key = (a.path.clone(), a.extent);
        let entry = last_for_key.entry(key).or_insert(0);
        if a.line_number >= *entry {
            *entry = a.line_number;
        }
    }
    staging
        .adds
        .retain(|a| last_for_key.get(&(a.path.clone(), a.extent)).copied() == Some(a.line_number));
    staging
}

fn extent_sort_key(extent: &AnchorExtent) -> (u32, u32) {
    match *extent {
        AnchorExtent::WholeFile => (0, 0),
        AnchorExtent::LineRange { start, end } => (start, end),
    }
}

fn path_exists_in_tree(repo: &gix::Repository, commit_sha: &str, path: &str) -> bool {
    matches!(
        crate::git::tree_entry_at(repo, commit_sha, std::path::Path::new(path)),
        Ok(Some(_))
    )
}
