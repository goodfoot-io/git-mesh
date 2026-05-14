//! Mesh commit pipeline — §6.1, §6.2.

use crate::anchor::create_anchor_with_extent_skipping_blob_bounds;
use crate::git::{self, resolve_ref_oid_optional, work_dir};
use crate::mesh::catalog::{Catalog, CATALOG_REF};
use crate::staging::{self, StagedConfig, Staging};
use crate::types::{AnchorExtent, MeshConfig};
use crate::validation::validate_mesh_name;
use crate::{Error, Result};

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

pub fn commit_mesh(repo: &gix::Repository, name: &str) -> Result<String> {
    validate_mesh_name(name)?;
    let wd = work_dir(repo)?;
    let staging = staging::read_staging(repo, name)?;

    // Detect loose-ref file vs. directory collisions BEFORE any ref I/O.
    // `try_find_reference` on `refs/meshes/v1/<name>` short-reads when an
    // ancestor path is already a regular file (committed leaf), so the
    // structured collision error must beat `resolve_ref_oid_optional` to
    // surface a useful message.
    check_prefix_collision(repo, name)?;

    // Load current state — try catalog first, fall back to per-mesh ref.
    let (anchor_v2_records, base_config, base_message) = {
        let cat = Catalog::load(repo)?;
        if !cat.is_empty() {
            match cat.lookup(name)? {
                Some(m) => (m.anchors_v2, m.config, Some(m.message)),
                None => (
                    Vec::new(),
                    MeshConfig {
                        copy_detection: crate::types::DEFAULT_COPY_DETECTION,
                        ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
                        follow_moves: crate::types::DEFAULT_FOLLOW_MOVES,
                    },
                    None,
                ),
            }
        } else {
            // Pre-catalog path: read from per-mesh ref.
            let mesh_ref_name = mesh_ref(name);
            let base_tip = resolve_ref_oid_optional(wd, &mesh_ref_name)?;
            match base_tip.as_deref() {
                Some(tip) => {
                    let m = super::read::read_mesh_at(repo, name, Some(tip))?;
                    (m.anchors_v2, m.config, Some(m.message))
                }
                None => (
                    Vec::new(),
                    MeshConfig {
                        copy_detection: crate::types::DEFAULT_COPY_DETECTION,
                        ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
                        follow_moves: crate::types::DEFAULT_FOLLOW_MOVES,
                    },
                    None,
                ),
            }
        }
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
    let (new_cd, new_iw, new_fm) = staging::resolve_staged_config(
        &staging,
        (base_config.copy_detection, base_config.ignore_whitespace, base_config.follow_moves),
    );
    new_config.copy_detection = new_cd;
    new_config.ignore_whitespace = new_iw;
    new_config.follow_moves = new_fm;

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
                StagedConfig::FollowMoves(b) => ("follow-moves", b.to_string()),
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
    const FOLLOW_SUBJECT_PREFIX: &str = "mesh: follow ";
    let message = match (&staging.why, &base_message) {
        (Some(m), _) => {
            // Reject a staged why whose first line begins with the reserved
            // prefix used by auto-follow commits. Allowing it would cause
            // `why_walking_past_follows` to silently skip the user's commit
            // and surface an older why instead.
            if m.lines().next().unwrap_or("").starts_with(FOLLOW_SUBJECT_PREFIX) {
                return Err(Error::ReservedWhyPrefix {
                    prefix: FOLLOW_SUBJECT_PREFIX.to_string(),
                });
            }
            m.clone()
        }
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

    // Build the anchor list once (same for all retries).
    let mut combined: Vec<(String, crate::types::Anchor)> = snapshots.clone();
    for (id, r) in &new_anchors {
        combined.push((id.clone(), r.clone()));
    }
    combined.sort_by(|a, b| {
        (a.1.path.as_str(), extent_sort_key(&a.1.extent))
            .cmp(&(b.1.path.as_str(), extent_sort_key(&b.1.extent)))
    });

    const MAX_RETRIES: usize = 5;
    let new_commit: String;
    let mut attempt: usize = 0;
    loop {
        let current_ref =
            crate::git::resolve_ref_oid_optional_repo(repo, CATALOG_REF)?;
        let mut catalog = Catalog::load(repo)?;

        let mesh = crate::mesh::catalog::build_mesh(
            name, &message, &combined, &new_config,
        );
        catalog.insert(name, &mesh)?;

        match crate::mesh::catalog::commit_catalog(
            repo,
            &catalog,
            &message,
            current_ref.as_deref(),
        ) {
            Ok(commit_oid) => {
                new_commit = commit_oid;
                break;
            }
            Err(_e) => {
                attempt += 1;
                if attempt >= MAX_RETRIES {
                    let found =
                        crate::git::resolve_ref_oid_optional_repo(repo, CATALOG_REF)?
                            .unwrap_or_default();
                    return Err(Error::ConcurrentUpdate {
                        expected: current_ref.unwrap_or_default(),
                        found,
                    });
                }
                // CAS conflict — reload catalog and retry.
                continue;
            }
        }
    }

    // Update path index refs now that the catalog commit succeeded.
    // These are independent CAS updates (not part of the catalog
    // transaction), so failures here do not undo the catalog write.
    let path_updates = super::path_index::ref_updates_for_mesh(
        repo, name, &anchor_v2_records, &combined,
    )?;
    // `ref_updates_for_mesh` already elides no-op updates (same
    // old and new blob content), so any remaining updates are real.
    if !path_updates.is_empty() {
        crate::git::ensure_log_all_ref_updates_always(repo)?;
        let _ = crate::git::apply_ref_transaction_repo(repo, &path_updates);
    }

    // Update per-mesh convenience ref so `git log refs/meshes/v1/<name>` works.
    // Use `apply_ref_transaction` (subprocess) rather than
    // `apply_ref_transaction_repo` (gix) because gix caches the git config
    // from repo-open time and won't see the `core.logAllRefUpdates = always`
    // value written by `ensure_log_all_ref_updates_always`.
    let mesh_ref_name = mesh_ref(name);
    crate::git::ensure_log_all_ref_updates_always(repo)?;
    {
        match resolve_ref_oid_optional(wd, &mesh_ref_name)? {
            Some(old_oid) => {
                let _ = crate::git::apply_ref_transaction(
                    wd,
                    &[crate::git::RefUpdate::Update {
                        name: mesh_ref_name,
                        new_oid: new_commit.clone(),
                        expected_old_oid: old_oid,
                    }],
                );
            }
            None => {
                let _ = crate::git::apply_ref_transaction(
                    wd,
                    &[crate::git::RefUpdate::Create {
                        name: mesh_ref_name,
                        new_oid: new_commit.clone(),
                    }],
                );
            }
        }
    }

    // Clear staging on success.
    let _ = staging::clear_staging(repo, name);

    Ok(new_commit)
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

/// Refuse a commit whose name would force the loose-ref tree to hold the
/// same path as both a regular file and a directory. The collision is
/// symmetric: `name = "a/b"` cannot coexist with `"a"`, and `name = "a"`
/// cannot coexist with `"a/b"`. Reports the first existing mesh that
/// blocks the write so the user can rename one side.
fn check_prefix_collision(repo: &gix::Repository, name: &str) -> Result<()> {
    let existing = git::list_refs_stripped(repo, "refs/meshes/v1")?;
    for other in existing {
        if other == name {
            continue;
        }
        // `other` is a strict ancestor of `name`: file blocks future directory.
        if let Some(rest) = name.strip_prefix(&other)
            && rest.starts_with('/')
        {
            return Err(Error::MeshNameCollidesWithExistingMesh {
                staged: name.to_string(),
                blocking: other,
            });
        }
        // `other` is a strict descendant of `name`: existing directory
        // blocks future file.
        if let Some(rest) = other.strip_prefix(name)
            && rest.starts_with('/')
        {
            return Err(Error::MeshNameCollidesWithExistingMesh {
                staged: name.to_string(),
                blocking: other,
            });
        }
    }
    Ok(())
}

fn path_exists_in_tree(repo: &gix::Repository, commit_sha: &str, path: &str) -> bool {
    matches!(
        crate::git::tree_entry_at(repo, commit_sha, std::path::Path::new(path)),
        Ok(Some(_))
    )
}
