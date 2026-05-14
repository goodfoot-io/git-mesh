//! Persistent path-anchor index keyed by `catalog_tree_oid + key_salt`.
//!
//! The index maps every path referenced by any anchor in the catalog
//! to the list of `(mesh, anchor_id, anchor_sha, blob_oid, extent,
//! config_hash)` tuples that participate in it. The dirty-overlay path
//! uses [`PathAnchorIndex::lookup_many`] to find the anchors affected
//! by a small set of dirty paths in `O(P_dirty)`.

use super::db::{Phase3Store, now_secs};
use super::dto::AnchorExtentDto;
use super::keys::{KEY_SALT, path_anchor_index_key};
use crate::types::{Mesh, MeshConfig};
use crate::{Error, Result};
use blake3::Hasher;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

const FORMAT_VERSION: u8 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AnchorIndexEntry {
    pub(crate) mesh_name: String,
    pub(crate) anchor_id: String,
    pub(crate) anchor_sha: String,
    pub(crate) blob_oid: String,
    pub(crate) extent: AnchorExtentDto,
    pub(crate) config_hash: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PathAnchorIndexDto {
    format_version: u8,
    catalog_tree_oid: String,
    by_path: Vec<(String, Vec<AnchorIndexEntry>)>,
}

/// Built form of the persistent index.
///
/// `by_path` keys are owned `Arc<[u8]>` so cloning the index for use
/// across threads is cheap. Tests construct this directly; the
/// resolver builds it from a catalog via [`build_path_anchor_index`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathAnchorIndex {
    pub(crate) catalog_tree_oid: String,
    pub(crate) by_path: HashMap<Arc<[u8]>, Vec<AnchorIndexEntry>>,
}

impl PathAnchorIndex {
    /// Look up the anchor entries for `paths` and return them grouped
    /// by path. Missing paths simply yield no entries.
    pub(crate) fn lookup_many<'a, I, S>(&'a self, paths: I) -> Vec<&'a AnchorIndexEntry>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let mut out: Vec<&'a AnchorIndexEntry> = Vec::new();
        for p in paths {
            if let Some(entries) = self.by_path.get(p.as_ref()) {
                out.extend(entries.iter());
            }
        }
        out
    }

    /// Anchors-affected count (sum of per-path entries) over `paths`.
    pub(crate) fn affected_anchor_count<I, S>(&self, paths: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let mut n = 0;
        for p in paths {
            if let Some(entries) = self.by_path.get(p.as_ref()) {
                n += entries.len();
            }
        }
        n
    }

    fn to_dto(&self) -> PathAnchorIndexDto {
        let mut by_path: Vec<(String, Vec<AnchorIndexEntry>)> = self
            .by_path
            .iter()
            .map(|(k, v)| {
                let key = String::from_utf8_lossy(k).into_owned();
                (key, v.clone())
            })
            .collect();
        by_path.sort_by(|a, b| a.0.cmp(&b.0));
        PathAnchorIndexDto {
            format_version: FORMAT_VERSION,
            catalog_tree_oid: self.catalog_tree_oid.clone(),
            by_path,
        }
    }

    fn from_dto(dto: PathAnchorIndexDto) -> Result<Self> {
        if dto.format_version != FORMAT_VERSION {
            return Err(Error::Git(format!(
                "phase3 path_anchor_index: format_version {} != expected {}",
                dto.format_version, FORMAT_VERSION
            )));
        }
        let mut by_path: HashMap<Arc<[u8]>, Vec<AnchorIndexEntry>> = HashMap::new();
        for (path, entries) in dto.by_path {
            let key: Arc<[u8]> = Arc::from(path.into_bytes().into_boxed_slice());
            by_path.insert(key, entries);
        }
        Ok(Self {
            catalog_tree_oid: dto.catalog_tree_oid,
            by_path,
        })
    }
}

/// Stable hash of a `MeshConfig`. Anchors with different mesh configs
/// drive distinct baseline resolutions, so the config hash is part of
/// each `AnchorIndexEntry` and feeds the overlay key.
pub(crate) fn config_hash(config: &MeshConfig) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.mesh-config\0");
    h.update(&[copy_detection_byte(config.copy_detection)]);
    h.update(&[u8::from(config.ignore_whitespace)]);
    h.update(&[u8::from(config.follow_moves)]);
    *h.finalize().as_bytes()
}

fn copy_detection_byte(c: crate::types::CopyDetection) -> u8 {
    use crate::types::CopyDetection::*;
    match c {
        Off => 0,
        SameCommit => 1,
        AnyFileInCommit => 2,
        AnyFileInRepo => 3,
    }
}

/// Build a [`PathAnchorIndex`] from an iterator of `(name, Mesh)`
/// pairs and a catalog tree oid. This walks every `Anchor` in every
/// mesh exactly once.
pub(crate) fn build_path_anchor_index<I>(catalog_tree_oid: &str, meshes: I) -> PathAnchorIndex
where
    I: IntoIterator<Item = (String, Mesh)>,
{
    let mut by_path: HashMap<Arc<[u8]>, Vec<AnchorIndexEntry>> = HashMap::new();
    for (_, mesh) in meshes {
        let cfg_hash = config_hash(&mesh.config);
        for (anchor_id, anchor) in &mesh.anchors_v2 {
            let key: Arc<[u8]> = Arc::from(anchor.path.as_bytes().to_vec().into_boxed_slice());
            by_path
                .entry(key)
                .or_default()
                .push(AnchorIndexEntry {
                    mesh_name: mesh.name.clone(),
                    anchor_id: anchor_id.clone(),
                    anchor_sha: anchor.anchor_sha.clone(),
                    blob_oid: anchor.blob.clone(),
                    extent: anchor.extent.into(),
                    config_hash: cfg_hash,
                });
        }
    }
    PathAnchorIndex {
        catalog_tree_oid: catalog_tree_oid.to_string(),
        by_path,
    }
}

/// Persist `index` into `store`, keyed by `catalog_tree_oid + KEY_SALT`.
pub(crate) fn store_path_anchor_index(store: &Phase3Store, index: &PathAnchorIndex) -> Result<()> {
    let (tree_oid, salt) = path_anchor_index_key(&index.catalog_tree_oid);
    let dto = index.to_dto();
    let payload = bincode::serialize(&dto)
        .map_err(|e| Error::Git(format!("phase3 path_anchor_index serialize: {e}")))?;
    let _ = KEY_SALT;
    store
        .conn
        .execute(
            "INSERT OR REPLACE INTO path_anchor_index \
             (catalog_tree_oid, key_salt, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![tree_oid, salt, payload, now_secs()],
        )
        .map_err(|e| Error::Git(format!("phase3 path_anchor_index insert: {e}")))?;
    Ok(())
}

/// Load the persistent index for `catalog_tree_oid` if present.
pub(crate) fn load_path_anchor_index(
    store: &Phase3Store,
    catalog_tree_oid: &str,
) -> Result<Option<PathAnchorIndex>> {
    let (tree_oid, salt) = path_anchor_index_key(catalog_tree_oid);
    let payload: Option<Vec<u8>> = store
        .conn
        .query_row(
            "SELECT payload FROM path_anchor_index \
             WHERE catalog_tree_oid = ?1 AND key_salt = ?2",
            rusqlite::params![tree_oid, salt],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::Git(format!("phase3 path_anchor_index select: {e}")))?;
    let Some(bytes) = payload else {
        return Ok(None);
    };
    let dto: PathAnchorIndexDto = match bincode::deserialize(&bytes) {
        Ok(d) => d,
        // Corrupt or shape-mismatched rows are misses. We do not error
        // out — the cache is a best-effort accelerator.
        Err(_) => return Ok(None),
    };
    match PathAnchorIndex::from_dto(dto) {
        Ok(idx) => Ok(Some(idx)),
        Err(_) => Ok(None),
    }
}
