//! Mesh commit pipeline — §6.1, §6.2.

use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::read::{parse_config_blob, serialize_config_blob};
use crate::range::{create_range_with_extent, read_range};
use crate::staging::{self, StagedConfig, Staging};
use crate::types::{Mesh, MeshConfig, RangeExtent};
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
    let (range_ids, base_config, base_message) = match base_tip.as_deref() {
        Some(tip) => {
            let m = super::read::read_mesh_at(repo, name, Some(tip))?;
            (m.ranges, m.config, Some(m.message))
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
    // materialized snapshot `(range_id, path, extent)`.
    let mut snapshots: Vec<(String, String, RangeExtent)> = Vec::with_capacity(range_ids.len());
    for id in &range_ids {
        let r = read_range(repo, id)?;
        snapshots.push((id.clone(), r.path, r.extent));
    }
    for rem in &staging.removes {
        let idx = snapshots
            .iter()
            .position(|(_, p, e)| p == &rem.path && *e == rem.extent)
            .ok_or_else(|| Error::RangeNotInMesh {
                path: rem.path.clone(),
                start: rem.start(),
                end: rem.end(),
            })?;
        snapshots.remove(idx);
    }
    // Adds that collide with an existing range at `(path, extent)` are
    // dedup-overrides per §D5: drop the prior snapshot, keep the staged
    // add.
    for a in &staging.adds {
        if let Some(idx) = snapshots
            .iter()
            .position(|(_, p, e)| p == &a.path && *e == a.extent)
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
    let meaningful_message = staging.message.is_some();

    if !meaningful_adds && !meaningful_removes && !config_changed && !meaningful_message {
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

    // Determine the commit message.
    let message = match (&staging.message, &base_message) {
        (Some(m), _) => m.clone(),
        (None, Some(prior)) => prior.clone(),
        (None, None) => return Err(Error::MessageRequired(name.into())),
    };

    // Drift check and range creation for staged adds. All-or-nothing:
    // create range refs for each add; on any failure propagate.
    let head_sha = git::head_oid(repo)?;
    let mut new_range_ids: Vec<String> = Vec::new();
    // Pre-validate every add against its resolved anchor (prevent partial
    // writes) BEFORE creating any range refs.
    for a in &staging.adds {
        let anchor = a.anchor.clone().unwrap_or_else(|| head_sha.clone());
        match a.extent {
            RangeExtent::Lines { start, end } => {
                let blob = crate::git::path_blob_at(repo, &anchor, &a.path)?;
                let line_count = crate::git::blob_line_count(repo, &blob)?;
                if start < 1 || end < start || end > line_count {
                    return Err(Error::InvalidRange { start, end });
                }
            }
            RangeExtent::Whole => {
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
        let id = create_range_with_extent(repo, &anchor, &a.path, a.extent)?;
        new_range_ids.push(id);
    }

    // CAS retry loop (§6). Range blobs are content-addressed and already
    // written; only the tree/commit/ref-update step needs retrying. On
    // conflict, reload the mesh tip, re-validate post-remove collisions
    // against the new snapshot, rebuild the tree/commit with the new
    // parent, and retry.
    let mut current_parent = base_tip.clone();
    let mut current_snapshots = snapshots;
    const MAX_RETRIES: usize = 5;
    let new_commit: String;
    let mut attempt: usize = 0;
    loop {
        // Combine ranges and canonicalize by (path, extent).
        let mut combined: Vec<(String, String, RangeExtent)> = current_snapshots.clone();
        for id in &new_range_ids {
            let r = read_range(repo, id)?;
            combined.push((id.clone(), r.path, r.extent));
        }
        combined.sort_by(|a, b| {
            (a.1.as_str(), extent_sort_key(&a.2))
                .cmp(&(b.1.as_str(), extent_sort_key(&b.2)))
        });
        let final_ids: Vec<String> = combined.iter().map(|(id, _, _)| id.clone()).collect();

        // Build tree: `ranges` blob + `config` blob.
        let ranges_text: String = {
            let mut s = String::new();
            for id in &final_ids {
                s.push_str(id);
                s.push('\n');
            }
            s
        };
        let ranges_blob = git::write_blob_bytes(repo, ranges_text.as_bytes())?;
        let config_text = serialize_config_blob(&new_config);
        let config_blob = git::write_blob_bytes(repo, config_text.as_bytes())?;
        // Build a tree with `config` and `ranges` entries. `git mktree`
        // sorts entries by name; gix expects them pre-sorted as well.
        let tree = Tree {
            entries: vec![
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: "config".into(),
                    oid: config_blob
                        .parse()
                        .map_err(|e| crate::Error::Git(format!("parse config blob oid: {e}")))?,
                },
                Entry {
                    mode: EntryKind::Blob.into(),
                    filename: "ranges".into(),
                    oid: ranges_blob
                        .parse()
                        .map_err(|e| crate::Error::Git(format!("parse ranges blob oid: {e}")))?,
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

        // Atomic CAS update.
        let update = match current_parent.as_deref() {
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
        match apply_ref_transaction(wd, &[update]) {
            Ok(()) => {
                new_commit = candidate;
                break;
            }
            Err(e) => {
                attempt += 1;
                if attempt >= MAX_RETRIES {
                    return Err(Error::ConcurrentUpdate {
                        expected: current_parent.clone().unwrap_or_default(),
                        found: resolve_ref_oid_optional(wd, &mesh_ref)?.unwrap_or_default(),
                    });
                }
                // Re-read the tip. If it hasn't actually changed, the
                // error wasn't a CAS conflict — surface it.
                let latest = resolve_ref_oid_optional(wd, &mesh_ref)?;
                if latest == current_parent {
                    return Err(e);
                }
                current_parent = latest;
                // Re-materialize snapshot from new tip and re-run
                // post-remove / add collision validation.
                let new_snapshots = match current_parent.as_deref() {
                    Some(tip) => {
                        let m = super::read::read_mesh_at(repo, name, Some(tip))?;
                        let mut out = Vec::with_capacity(m.ranges.len());
                        for id in &m.ranges {
                            let r = read_range(repo, id)?;
                            out.push((id.clone(), r.path, r.extent));
                        }
                        out
                    }
                    None => Vec::new(),
                };
                let mut post = new_snapshots;
                for rem in &staging.removes {
                    let idx = post
                        .iter()
                        .position(|(_, p, e)| p == &rem.path && *e == rem.extent)
                        .ok_or_else(|| Error::RangeNotInMesh {
                            path: rem.path.clone(),
                            start: rem.start(),
                            end: rem.end(),
                        })?;
                    post.remove(idx);
                }
                // Adds at the same `(path, extent)` as an existing range
                // are treated as last-write-wins overrides (plan §D5).
                for a in &staging.adds {
                    if let Some(idx) = post
                        .iter()
                        .position(|(_, p, e)| p == &a.path && *e == a.extent)
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
    // Rebuild file index.
    let _ = crate::file_index::rebuild_index(repo);

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
    let mut last_for_key: HashMap<(String, RangeExtent), u32> = HashMap::new();
    for a in &staging.adds {
        let key = (a.path.clone(), a.extent);
        let entry = last_for_key.entry(key).or_insert(0);
        if a.line_number >= *entry {
            *entry = a.line_number;
        }
    }
    staging.adds.retain(|a| {
        last_for_key
            .get(&(a.path.clone(), a.extent))
            .copied()
            == Some(a.line_number)
    });
    staging
}

fn extent_sort_key(extent: &RangeExtent) -> (u32, u32) {
    match *extent {
        RangeExtent::Whole => (0, 0),
        RangeExtent::Lines { start, end } => (start, end),
    }
}

fn path_exists_in_tree(repo: &gix::Repository, commit_sha: &str, path: &str) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-tree", commit_sha, "--", path])
        .output();
    matches!(out, Ok(o) if o.status.success() && !o.stdout.is_empty())
}
