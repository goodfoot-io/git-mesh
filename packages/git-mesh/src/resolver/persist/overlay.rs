//! Dirty overlay: detect dirty paths, fingerprint them, build the
//! overlay cache key, and merge a partial re-resolution back onto a
//! committed baseline.
//!
//! ## Merge semantics
//!
//! The committed baseline is the HEAD-only resolution of every mesh in
//! the catalog. An overlay resolution covers the meshes containing at
//! least one anchor whose path is dirty in the index, worktree, or
//! staging. Merging the overlay onto the baseline replaces each
//! affected mesh in the baseline with the overlay's version, preserving
//! the baseline ordering for unaffected meshes.
//!
//! ## Dirty path detection
//!
//! The set of dirty paths is the union of:
//!
//! * `LayerStatus::worktree_paths` — paths the worktree differs from
//!   the index on.
//! * Paths whose stage-0 index entry differs from HEAD (we model this
//!   via `index_dirty` plus the index checksum: when `index_dirty` is
//!   true the entire index is included in the fingerprint via its
//!   trailer SHA so any index-side change invalidates the overlay key
//!   even if we can't enumerate the exact paths without re-reading the
//!   index).
//! * Conflicted paths.
//! * Paths referenced by any sidecar in `.git/mesh/staging/`.
//!
//! When `LayerStatus::requires_full_scan` is set the overlay is not
//! used: we fall back to a full resolution because we can't bound the
//! dirty path set.

use super::baseline::CommittedBaseline;
use super::db::{Phase3Store, now_secs};
use super::dto::MeshResolvedDto;
use super::keys::dirty_overlay_key;
use crate::types::MeshResolved;
use crate::{Error, Result};
use blake3::Hasher;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

const FORMAT_VERSION: u8 = 1;

/// All inputs needed to compute the overlay key. The runtime path
/// constructs this from the engine's layer status; tests construct it
/// directly with synthetic values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OverlayInputs {
    pub(crate) catalog_tree_oid: String,
    pub(crate) head_oid: String,
    pub(crate) filter_config_hash: [u8; 32],
    pub(crate) index_checksum: [u8; 32],
    pub(crate) worktree_dirty_fingerprint: [u8; 32],
    pub(crate) staging_state_fingerprint: [u8; 32],
}

impl OverlayInputs {
    pub(crate) fn key(&self) -> [u8; 32] {
        dirty_overlay_key(
            &self.catalog_tree_oid,
            &self.head_oid,
            &self.filter_config_hash,
            &self.index_checksum,
            &self.worktree_dirty_fingerprint,
            &self.staging_state_fingerprint,
        )
    }
}

/// Collected dirty path set from the layers + staging.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirtyPaths {
    pub(crate) paths: BTreeSet<String>,
    /// True when we couldn't bound the set (rename detection blew out,
    /// status was unparseable, etc.) and the caller must fall back to
    /// a full resolution.
    pub(crate) requires_full_scan: bool,
}

/// Compute the dirty path set + an overlay-input bundle from the
/// engine's pre-computed layer state.
///
/// `index_trailer` is the 20-byte SHA-1 trailer of `.git/index` (the
/// engine reads it at session start; we hash it here to fingerprint
/// the index).
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_dirty_paths(
    catalog_tree_oid: &str,
    head_oid: &str,
    filter_config_hash: [u8; 32],
    index_trailer: Option<[u8; 20]>,
    index_dirty: bool,
    worktree_paths: &HashSet<String>,
    conflicted_paths: &HashSet<String>,
    staging_dir: Option<&Path>,
    requires_full_scan: bool,
) -> (DirtyPaths, OverlayInputs) {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    paths.extend(worktree_paths.iter().cloned());
    paths.extend(conflicted_paths.iter().cloned());
    let (staging_paths, staging_fp) = staging_fingerprint(staging_dir);
    paths.extend(staging_paths);
    let index_checksum = index_checksum_bytes(index_trailer, index_dirty);
    let worktree_fp = worktree_dirty_fingerprint(worktree_paths, conflicted_paths);
    let inputs = OverlayInputs {
        catalog_tree_oid: catalog_tree_oid.to_string(),
        head_oid: head_oid.to_string(),
        filter_config_hash,
        index_checksum,
        worktree_dirty_fingerprint: worktree_fp,
        staging_state_fingerprint: staging_fp,
    };
    (
        DirtyPaths {
            paths,
            requires_full_scan,
        },
        inputs,
    )
}

/// Fingerprint of the `.git/index` for overlay-key purposes.
///
/// * Clean index → all-zero digest. Two invocations against the same
///   clean index produce identical fingerprints.
/// * Dirty index with a known trailer → BLAKE3(trailer || "dirty"). The
///   trailer changes whenever the index changes, so the digest also
///   changes.
/// * Dirty index with no readable trailer → BLAKE3 of a sentinel +
///   wall-clock seconds, ensuring the overlay key won't collide across
///   invocations (we can't be sure two unreadable-trailer states are
///   equivalent).
fn index_checksum_bytes(trailer: Option<[u8; 20]>, index_dirty: bool) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.index-checksum\0");
    match (index_dirty, trailer) {
        (false, _) => {
            h.update(&[0u8]);
        }
        (true, Some(t)) => {
            h.update(&[1u8]);
            h.update(&t);
        }
        (true, None) => {
            h.update(&[2u8]);
            h.update(&now_secs().to_le_bytes());
        }
    }
    *h.finalize().as_bytes()
}

fn worktree_dirty_fingerprint(
    worktree_paths: &HashSet<String>,
    conflicted: &HashSet<String>,
) -> [u8; 32] {
    let mut items: Vec<&str> = worktree_paths
        .iter()
        .map(|s| s.as_str())
        .chain(conflicted.iter().map(|s| s.as_str()))
        .collect();
    items.sort();
    items.dedup();
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.worktree-dirty\0");
    for p in items {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    *h.finalize().as_bytes()
}

/// Walk `.git/mesh/staging/<mesh>/` directories and produce
/// (paths-touched-by-sidecars, fingerprint). The fingerprint covers
/// every staged op's anchor address plus the sidecar bytes' mtime so
/// editing a sidecar in place invalidates the overlay key.
fn staging_fingerprint(staging_dir: Option<&Path>) -> (Vec<String>, [u8; 32]) {
    let mut paths: Vec<String> = Vec::new();
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.staging-fingerprint\0");
    let Some(dir) = staging_dir else {
        h.update(&[0u8]);
        return (paths, *h.finalize().as_bytes());
    };
    h.update(&[1u8]);
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let mesh_name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            entries.push((mesh_name, path));
        }
    }
    entries.sort();
    for (mesh, path) in entries {
        write_prefixed(&mut h, mesh.as_bytes());
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&path) {
            for ent in rd.flatten() {
                files.push(ent.path());
            }
        }
        files.sort();
        for f in files {
            let name = f
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            write_prefixed(&mut h, name.as_bytes());
            if let Ok(md) = std::fs::metadata(&f) {
                h.update(&md.len().to_le_bytes());
                if let Ok(mt) = md.modified()
                    && let Ok(d) =
                        mt.duration_since(std::time::SystemTime::UNIX_EPOCH)
                {
                    h.update(&d.as_nanos().to_le_bytes());
                }
            }
            // For `.add` / `.remove` ops, the first line is the anchor
            // address `<path>#L<start>-L<end>`. We hash the whole file
            // anyway via length+mtime above, but we also peek the first
            // line so the affected path can be added to `paths`.
            if let Ok(bytes) = std::fs::read(&f)
                && let Some(first_line) = bytes.split(|b| *b == b'\n').next()
                && let Ok(s) = std::str::from_utf8(first_line)
                && let Some(path_part) = extract_address_path(s)
            {
                paths.push(path_part.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    (paths, *h.finalize().as_bytes())
}

fn extract_address_path(line: &str) -> Option<&str> {
    // `path#Lstart-Lend` → split on the last '#L'. Whole-file ops have
    // no `#L`; treat the line as the path verbatim. Lines that contain
    // additional fields after a tab are truncated at the tab.
    let line = line.split('\t').next().unwrap_or(line);
    if let Some(idx) = line.rfind("#L") {
        Some(&line[..idx])
    } else if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

fn write_prefixed(h: &mut Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// In-process overlay value: the partial mesh resolution to merge onto
/// the baseline. `affected_meshes` lists the mesh names that the
/// overlay covers; meshes not in the list keep their baseline value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirtyOverlay {
    pub(crate) affected_meshes: Vec<String>,
    pub(crate) meshes: Vec<MeshResolved>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DirtyOverlayDto {
    format_version: u8,
    affected_meshes: Vec<String>,
    meshes: Vec<MeshResolvedDto>,
}

pub(crate) fn store_overlay(
    store: &Phase3Store,
    inputs: &OverlayInputs,
    overlay: &DirtyOverlay,
) -> Result<()> {
    let key = inputs.key();
    let dto = DirtyOverlayDto {
        format_version: FORMAT_VERSION,
        affected_meshes: overlay.affected_meshes.clone(),
        meshes: overlay.meshes.iter().map(Into::into).collect(),
    };
    let payload = bincode::serialize(&dto)
        .map_err(|e| Error::Git(format!("phase3 dirty_overlay serialize: {e}")))?;
    store
        .conn
        .execute(
            "INSERT OR REPLACE INTO dirty_overlay (overlay_key, payload, created_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![key.to_vec(), payload, now_secs()],
        )
        .map_err(|e| Error::Git(format!("phase3 dirty_overlay insert: {e}")))?;
    Ok(())
}

pub(crate) fn load_overlay(
    store: &Phase3Store,
    inputs: &OverlayInputs,
) -> Result<Option<DirtyOverlay>> {
    let key = inputs.key();
    let payload: Option<Vec<u8>> = store
        .conn
        .query_row(
            "SELECT payload FROM dirty_overlay WHERE overlay_key = ?1",
            rusqlite::params![key.to_vec()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| Error::Git(format!("phase3 dirty_overlay select: {e}")))?;
    let Some(bytes) = payload else {
        return Ok(None);
    };
    let dto: DirtyOverlayDto = match bincode::deserialize(&bytes) {
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
    Ok(Some(DirtyOverlay {
        affected_meshes: dto.affected_meshes,
        meshes,
    }))
}

/// Compute the public-facing worktree dirty fingerprint. Exposed
/// separately so tests can assert that two semantically-equivalent
/// dirty sets produce the same fingerprint.
pub(crate) fn overlay_dirty_fingerprint(
    worktree_paths: &HashSet<String>,
    conflicted_paths: &HashSet<String>,
) -> [u8; 32] {
    worktree_dirty_fingerprint(worktree_paths, conflicted_paths)
}

/// Apply an overlay to a baseline, producing a final `Vec<MeshResolved>`.
///
/// Meshes named in `overlay.affected_meshes` are replaced by the
/// overlay's version; any mesh in `overlay.meshes` whose name is not
/// in the baseline is appended (e.g. a newly-added mesh seen only in
/// the overlay). Baseline order is preserved for unaffected meshes.
pub(crate) fn apply_overlay(
    baseline: &CommittedBaseline,
    overlay: &DirtyOverlay,
) -> Vec<MeshResolved> {
    let affected: HashSet<&str> = overlay.affected_meshes.iter().map(|s| s.as_str()).collect();
    let overlay_by_name: std::collections::HashMap<&str, &MeshResolved> =
        overlay.meshes.iter().map(|m| (m.name.as_str(), m)).collect();
    let mut out: Vec<MeshResolved> = Vec::with_capacity(baseline.meshes.len() + overlay.meshes.len());
    let mut seen: HashSet<String> = HashSet::new();
    for m in &baseline.meshes {
        if affected.contains(m.name.as_str()) {
            if let Some(rep) = overlay_by_name.get(m.name.as_str()) {
                seen.insert(m.name.clone());
                out.push((*rep).clone());
                continue;
            }
            // Affected but no replacement → drop the baseline mesh.
            // (Overlay said "this mesh resolves to empty after dirty
            // application", e.g. all anchors went to Fresh.)
            seen.insert(m.name.clone());
            continue;
        }
        out.push(m.clone());
    }
    for m in &overlay.meshes {
        if !seen.contains(&m.name) {
            out.push(m.clone());
        }
    }
    out
}
