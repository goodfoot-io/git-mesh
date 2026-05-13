//! Content-addressed FS cache for `git mesh stale`.
//!
//! Single polymorphic surface: a [`Cache`] backed by an L1 in-memory map and
//! an L2 on-disk store under `<common_dir>/mesh/cache/v1/<kind>/<aa>/<rest>`,
//! keyed by BLAKE3 of canonical key bytes. Three [`Kind`]s today:
//! `GroupedWalk`, `RenameTrail`, `DriftLocus`.
//!
//! Phase 1 (this file): the contract is declared as stubs. Runtime use of any
//! method beyond [`Cache::open_disabled`] / [`Cache::is_enabled`] panics via
//! `todo!()`. Phase 3 implements the bodies.
//!
//! See [`packages/git-mesh/plan/initial.md`](../../../plan/initial.md) for the
//! full design.

use crate::Result;
use crate::types::CopyDetection;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ── Kind ───────────────────────────────────────────────────────────────────

/// Discriminant for cache subdirectories. Used as both an L1 key component
/// and as the on-disk path segment via [`Kind::as_dir`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Kind {
    GroupedWalk,
    RenameTrail,
    DriftLocus,
}

impl Kind {
    pub fn as_dir(self) -> &'static str {
        match self {
            Kind::GroupedWalk => "grouped_walk",
            Kind::RenameTrail => "rename_trail",
            Kind::DriftLocus => "drift_locus",
        }
    }
}

// ── CacheKey trait ──────────────────────────────────────────────────────────

/// Trait implemented by each per-kind key struct. Implementors write a
/// domain-separation tag (`b"gm.v1.<kind>\0"`) followed by each field in
/// fixed network-byte-order. No `Hash`, no `bincode`, no serde for the key
/// path — the hash of `canonical_bytes` is the L1/L2 lookup key.
pub trait CacheKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>);
}

// ── Per-kind key structs ────────────────────────────────────────────────────

/// Key for the `grouped_walk` kind. Fields mirror the prior sqlite PK
/// tuple at the call site in [`crate::resolver::session::build_grouped_walk`].
pub struct GroupedWalkKey {
    pub anchor_sha: String,
    pub copy_detection: CopyDetection,
    pub seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
    pub rename_budget: i64,
    pub head_sha: String,
}

impl CacheKey for GroupedWalkKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"gm.v1.grouped_walk\0");
        write_str(out, &self.anchor_sha);
        out.push(copy_detection_byte(self.copy_detection));
        out.extend_from_slice(&self.seed_hash);
        out.extend_from_slice(&self.replace_refs_hash);
        out.extend_from_slice(&self.git_config_hash);
        out.extend_from_slice(&self.rename_budget.to_be_bytes());
        write_str(out, &self.head_sha);
    }
}

/// Key for the `rename_trail` kind. Fields mirror the prior `TrailCacheKey`.
pub struct RenameTrailKey {
    pub anchor_sha: String,
    pub head_sha: String,
    pub copy_detection: CopyDetection,
    pub rename_budget: i64,
    pub candidate_seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
}

impl CacheKey for RenameTrailKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"gm.v1.rename_trail\0");
        write_str(out, &self.anchor_sha);
        write_str(out, &self.head_sha);
        out.push(copy_detection_byte(self.copy_detection));
        out.extend_from_slice(&self.rename_budget.to_be_bytes());
        out.extend_from_slice(&self.candidate_seed_hash);
        out.extend_from_slice(&self.replace_refs_hash);
        out.extend_from_slice(&self.git_config_hash);
    }
}

/// Key for the `drift_locus` kind. Fields mirror the prior `DriftLocusCacheKey`.
pub struct DriftLocusKey {
    pub anchor_sha: String,
    pub path: String,
    pub blob_oid: String,
    pub range_start: u32,
    pub range_end: u32,
    pub copy_detection: CopyDetection,
    pub rename_budget: i64,
}

impl CacheKey for DriftLocusKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"gm.v1.drift_locus\0");
        write_str(out, &self.anchor_sha);
        write_str(out, &self.path);
        write_str(out, &self.blob_oid);
        out.extend_from_slice(&self.range_start.to_be_bytes());
        out.extend_from_slice(&self.range_end.to_be_bytes());
        out.push(copy_detection_byte(self.copy_detection));
        out.extend_from_slice(&self.rename_budget.to_be_bytes());
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn copy_detection_byte(cd: CopyDetection) -> u8 {
    match cd {
        CopyDetection::Off => 0,
        CopyDetection::SameCommit => 1,
        CopyDetection::AnyFileInCommit => 2,
        CopyDetection::AnyFileInRepo => 3,
    }
}

// ── GcStats ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GcStats {
    pub grouped_walk_removed: usize,
    pub rename_trail_removed: usize,
    pub drift_locus_removed: usize,
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// Two-tier content-addressed cache. L1 is a [`Mutex<HashMap>`] of serialized
/// payloads keyed by `(Kind, blake3_hash)`; L2 is an on-disk store rooted at
/// `dir`. When `enabled == false` (env `GIT_MESH_CACHE=0` or
/// [`Cache::open_disabled`]), every [`Cache::get_or_insert_with`] call routes
/// straight to `compute` and skips both tiers.
type L1Map = HashMap<(Kind, [u8; 32]), Arc<[u8]>>;

#[allow(dead_code)]
pub struct Cache {
    dir: PathBuf,
    l1: Mutex<L1Map>,
    enabled: bool,
}

impl Cache {
    /// Open the cache rooted at `<common_dir>/mesh/cache/v1`. Honors
    /// `GIT_MESH_CACHE=0` to short-circuit to a disabled cache.
    ///
    /// Phase 1: stub — `todo!()`.
    pub fn open(_repo: &gix::Repository) -> Result<Self> {
        todo!("Cache::open: Phase 3")
    }

    /// Return a permanently-disabled cache. Calls to
    /// [`Cache::get_or_insert_with`] route straight to `compute` with no
    /// L1 or L2 I/O.
    pub fn open_disabled() -> Self {
        Cache {
            dir: PathBuf::new(),
            l1: Mutex::new(HashMap::new()),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Cache the value produced by `compute` under `(kind, key)`. On L1 or L2
    /// hit, `compute` is not invoked; on miss, `compute` is invoked exactly
    /// once and the result is persisted to both tiers.
    ///
    /// Phase 1: stub — `todo!()`.
    pub fn get_or_insert_with<K, V, F>(&self, _kind: Kind, _key: &K, _compute: F) -> Result<V>
    where
        K: CacheKey,
        V: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<V>,
    {
        todo!("Cache::get_or_insert_with: Phase 3")
    }

    /// Sweep cache entries whose bound oids are no longer reachable.
    ///
    /// Phase 1: stub — `todo!()`.
    pub fn gc(&self, _repo: &gix::Repository) -> Result<GcStats> {
        todo!("Cache::gc: Phase 3")
    }
}

#[cfg(test)]
mod tests;
