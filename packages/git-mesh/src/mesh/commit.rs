//! Mesh commit pipeline — §6.1, §6.2.

use crate::git::{
    apply_ref_transaction, git_stdout, git_stdout_with_identity, git_with_input,
    resolve_ref_oid_optional, work_dir, RefUpdate,
};
use crate::mesh::read::{parse_config_blob, serialize_config_blob};
use crate::range::{create_range, read_range};
use crate::staging::{self, StagedConfig, Staging};
use crate::types::{Mesh, MeshConfig};
use crate::validation::validate_mesh_name;
use crate::{Error, Result};
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
        None => (Vec::new(), MeshConfig {
            copy_detection: crate::types::DEFAULT_COPY_DETECTION,
            ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
        }, None),
    };

    // Early: intra-staging duplicate-location detection.
    for (i, a) in staging.adds.iter().enumerate() {
        for b in &staging.adds[..i] {
            if a.path == b.path && a.start == b.start && a.end == b.end {
                return Err(Error::DuplicateRangeLocation {
                    path: a.path.clone(),
                    start: a.start,
                    end: a.end,
                });
            }
        }
    }

    // Validate removes exist and adds don't collide post-remove. Work on a
    // materialized snapshot `(range_id, path, start, end)` triples.
    let mut snapshots: Vec<(String, String, u32, u32)> = Vec::with_capacity(range_ids.len());
    for id in &range_ids {
        let r = read_range(repo, id)?;
        snapshots.push((id.clone(), r.path, r.start, r.end));
    }
    for rem in &staging.removes {
        let idx = snapshots
            .iter()
            .position(|(_, p, s, e)| p == &rem.path && *s == rem.start && *e == rem.end)
            .ok_or_else(|| Error::RangeNotInMesh {
                path: rem.path.clone(),
                start: rem.start,
                end: rem.end,
            })?;
        snapshots.remove(idx);
    }
    for a in &staging.adds {
        if snapshots
            .iter()
            .any(|(_, p, s, e)| p == &a.path && *s == a.start && *e == a.end)
        {
            return Err(Error::DuplicateRangeLocation {
                path: a.path.clone(),
                start: a.start,
                end: a.end,
            });
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
    let head_sha = git_stdout(wd, ["rev-parse", "HEAD"])?;
    let mut new_range_ids: Vec<String> = Vec::new();
    // Pre-validate every add against its resolved anchor (prevent partial
    // writes) BEFORE creating any range refs.
    for a in &staging.adds {
        let anchor = a.anchor.clone().unwrap_or_else(|| head_sha.clone());
        // Confirm the path is present at the anchor; fail closed before any write.
        let _ = crate::git::path_blob_at(repo, &anchor, &a.path)?;
        let blob = crate::git::path_blob_at(repo, &anchor, &a.path)?;
        let line_count = crate::git::blob_line_count(repo, &blob)?;
        if a.start < 1 || a.end < a.start || a.end > line_count {
            return Err(Error::InvalidRange {
                start: a.start,
                end: a.end,
            });
        }
    }
    for a in &staging.adds {
        let anchor = a.anchor.clone().unwrap_or_else(|| head_sha.clone());
        let id = create_range(repo, &anchor, &a.path, a.start, a.end)?;
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
        // Combine ranges and canonicalize by (path, start, end).
        let mut combined: Vec<(String, String, u32, u32)> = current_snapshots.clone();
        for id in &new_range_ids {
            let r = read_range(repo, id)?;
            combined.push((id.clone(), r.path, r.start, r.end));
        }
        combined.sort_by(|a, b| (a.1.as_str(), a.2, a.3).cmp(&(b.1.as_str(), b.2, b.3)));
        let final_ids: Vec<String> =
            combined.iter().map(|(id, _, _, _)| id.clone()).collect();

        // Build tree: `ranges` blob + `config` blob.
        let ranges_text: String = {
            let mut s = String::new();
            for id in &final_ids {
                s.push_str(id);
                s.push('\n');
            }
            s
        };
        let ranges_blob =
            git_with_input(wd, ["hash-object", "-w", "--stdin"], &ranges_text)?;
        let config_text = serialize_config_blob(&new_config);
        let config_blob =
            git_with_input(wd, ["hash-object", "-w", "--stdin"], &config_text)?;
        let tree_input = format!(
            "100644 blob {config_blob}\tconfig\n100644 blob {ranges_blob}\tranges\n"
        );
        let tree_oid = git_with_input(wd, ["mktree"], &tree_input)?;

        // Commit.
        let mut args = vec![
            "commit-tree".to_string(),
            tree_oid,
            "-m".to_string(),
            message.clone(),
        ];
        if let Some(parent) = current_parent.as_deref() {
            args.push("-p".to_string());
            args.push(parent.to_string());
        }
        let candidate = git_stdout_with_identity(wd, args.iter().map(String::as_str))?;

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
                        found: resolve_ref_oid_optional(wd, &mesh_ref)?
                            .unwrap_or_default(),
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
                            out.push((id.clone(), r.path, r.start, r.end));
                        }
                        out
                    }
                    None => Vec::new(),
                };
                let mut post = new_snapshots;
                for rem in &staging.removes {
                    let idx = post
                        .iter()
                        .position(|(_, p, s, e)| {
                            p == &rem.path && *s == rem.start && *e == rem.end
                        })
                        .ok_or_else(|| Error::RangeNotInMesh {
                            path: rem.path.clone(),
                            start: rem.start,
                            end: rem.end,
                        })?;
                    post.remove(idx);
                }
                for a in &staging.adds {
                    if post
                        .iter()
                        .any(|(_, p, s, e)| p == &a.path && *s == a.start && *e == a.end)
                    {
                        return Err(Error::DuplicateRangeLocation {
                            path: a.path.clone(),
                            start: a.start,
                            end: a.end,
                        });
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
