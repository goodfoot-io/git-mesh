//! Committed baseline persistence.
//!
//! A `CommittedBaseline` is the cached output of a HEAD-only resolution
//! of the entire catalog at a fixed `(catalog_tree_oid, head_oid,
//! filter_config_hash)`. It is what the warm same-HEAD path loads
//! before applying the dirty overlay.

use super::db::{Phase3Store, now_secs};
use super::dto::MeshResolvedDto;
use super::keys::{KEY_SALT, baseline_key};
use crate::types::MeshResolved;
use crate::{Error, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u8 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BaselineCounts {
    pub(crate) anchors_fresh: u64,
    pub(crate) anchors_moved: u64,
    pub(crate) anchors_changed: u64,
    pub(crate) anchors_orphaned: u64,
    pub(crate) anchors_merge_conflict: u64,
    pub(crate) anchors_unavailable: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CommittedBaselineDto {
    format_version: u8,
    catalog_tree_oid: String,
    head_oid: String,
    meshes: Vec<MeshResolvedDto>,
    counts: BaselineCounts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommittedBaseline {
    pub(crate) catalog_tree_oid: String,
    pub(crate) head_oid: String,
    pub(crate) meshes: Vec<MeshResolved>,
    pub(crate) counts: BaselineCounts,
}

pub(crate) fn store_baseline(
    store: &Phase3Store,
    filter_config_hash: &[u8; 32],
    baseline: &CommittedBaseline,
) -> Result<()> {
    let (tree_oid, head_oid, filter_hex, salt) = baseline_key(
        &baseline.catalog_tree_oid,
        &baseline.head_oid,
        filter_config_hash,
    );
    let dto = CommittedBaselineDto {
        format_version: FORMAT_VERSION,
        catalog_tree_oid: baseline.catalog_tree_oid.clone(),
        head_oid: baseline.head_oid.clone(),
        meshes: baseline.meshes.iter().map(Into::into).collect(),
        counts: baseline.counts.clone(),
    };
    let payload = bincode::serialize(&dto)
        .map_err(|e| Error::Git(format!("phase3 committed_baseline serialize: {e}")))?;
    let _ = KEY_SALT;
    store
        .conn
        .execute(
            "INSERT OR REPLACE INTO committed_baseline \
             (catalog_tree_oid, head_oid, filter_config_hash, key_salt, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![tree_oid, head_oid, filter_hex, salt, payload, now_secs()],
        )
        .map_err(|e| Error::Git(format!("phase3 committed_baseline insert: {e}")))?;
    Ok(())
}

pub(crate) fn load_baseline(
    store: &Phase3Store,
    catalog_tree_oid: &str,
    head_oid: &str,
    filter_config_hash: &[u8; 32],
) -> Result<Option<CommittedBaseline>> {
    let (tree_oid, head, filter_hex, salt) =
        baseline_key(catalog_tree_oid, head_oid, filter_config_hash);
    let payload: Option<Vec<u8>> = store
        .conn
        .query_row(
            "SELECT payload FROM committed_baseline \
             WHERE catalog_tree_oid = ?1 AND head_oid = ?2 \
               AND filter_config_hash = ?3 AND key_salt = ?4",
            rusqlite::params![tree_oid, head, filter_hex, salt],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::Git(format!("phase3 committed_baseline select: {e}")))?;
    let Some(bytes) = payload else {
        return Ok(None);
    };
    let dto: CommittedBaselineDto = match bincode::deserialize(&bytes) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    if dto.format_version != FORMAT_VERSION {
        return Ok(None);
    }
    let mut meshes = Vec::with_capacity(dto.meshes.len());
    for m in dto.meshes {
        match MeshResolved::try_from(m) {
            Ok(mr) => meshes.push(mr),
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(CommittedBaseline {
        catalog_tree_oid: dto.catalog_tree_oid,
        head_oid: dto.head_oid,
        meshes,
        counts: dto.counts,
    }))
}

/// Delete the cached baseline for this key. Used by tests; the runtime
/// invalidates by key rather than by deletion.
pub(crate) fn delete_baseline(
    store: &Phase3Store,
    catalog_tree_oid: &str,
    head_oid: &str,
    filter_config_hash: &[u8; 32],
) -> Result<()> {
    let (tree_oid, head, filter_hex, salt) =
        baseline_key(catalog_tree_oid, head_oid, filter_config_hash);
    store
        .conn
        .execute(
            "DELETE FROM committed_baseline \
             WHERE catalog_tree_oid = ?1 AND head_oid = ?2 \
               AND filter_config_hash = ?3 AND key_salt = ?4",
            rusqlite::params![tree_oid, head, filter_hex, salt],
        )
        .map_err(|e| Error::Git(format!("phase3 committed_baseline delete: {e}")))?;
    Ok(())
}

impl CommittedBaseline {
    pub(crate) fn counts_from_meshes(meshes: &[MeshResolved]) -> BaselineCounts {
        use crate::types::AnchorStatus::*;
        let mut c = BaselineCounts::default();
        for m in meshes {
            for a in &m.anchors {
                match &a.status {
                    Fresh => c.anchors_fresh += 1,
                    Moved => c.anchors_moved += 1,
                    Changed => c.anchors_changed += 1,
                    Orphaned => c.anchors_orphaned += 1,
                    MergeConflict => c.anchors_merge_conflict += 1,
                    Submodule | ContentUnavailable(_) => c.anchors_unavailable += 1,
                }
            }
        }
        c
    }
}
