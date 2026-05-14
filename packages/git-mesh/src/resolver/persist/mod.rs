//! Phase 3: persistent baseline and dirty-overlay cache for `git mesh stale`.
//!
//! This module provides cross-invocation persistence under
//! `$GIT_COMMON_DIR/mesh/stale-cache.db`, an SQLite database opened in
//! WAL mode. It stores three artifacts:
//!
//! 1. [`PathAnchorIndex`] — keyed by `catalog_tree_oid + key_salt`. Maps
//!    each path that any anchor refers to to the list of `(mesh, anchor_id,
//!    anchor_sha, blob_oid, extent, config_hash)` tuples it participates
//!    in. The dirty-overlay path uses this to find the anchors affected
//!    by a small set of dirty paths in `O(P_dirty)`.
//! 2. [`CommittedBaseline`] — keyed by
//!    `catalog_tree_oid + head_oid + filter_config_hash + key_salt`.
//!    Stores the full `Vec<MeshResolved>` for a HEAD-only resolution
//!    (no index, no worktree, no staged-mesh). Loaded by the warm
//!    same-HEAD path and merged with the dirty overlay.
//! 3. [`DirtyOverlay`] — keyed by
//!    `baseline_key + index_checksum + worktree_dirty_fingerprints +
//!    staging_state_fingerprint + filter_config_hash + key_salt`.
//!    Stores the overlay `Vec<MeshResolved>` for only the anchors
//!    affected by the dirty path set.
//!
//! ## key_salt
//!
//! [`KEY_SALT`] is a namespace constant, not a migration version. When
//! the on-disk shape changes the salt is bumped, old rows are simply
//! never read again, and a future gc pass can remove them. See
//! `three-phase-plan.md` §"Phase 3".

#![allow(dead_code, unused_imports)]

pub(crate) mod baseline;
pub(crate) mod db;
pub(crate) mod dto;
pub(crate) mod keys;
pub(crate) mod overlay;
pub(crate) mod path_anchor_index;

#[cfg(test)]
mod tests;

pub(crate) use baseline::{
    BaselineCounts, CommittedBaseline, delete_baseline, load_baseline, store_baseline,
};
pub(crate) use db::{Phase3Store, open_store};
pub(crate) use keys::{
    KEY_SALT, baseline_key, dirty_overlay_key, filter_config_hash, hex32, key_salt_le,
    path_anchor_index_key, salt_bytes,
};
pub(crate) use overlay::{
    DirtyOverlay, DirtyPaths, OverlayInputs, apply_overlay, collect_dirty_paths,
    load_overlay, overlay_dirty_fingerprint, store_overlay,
};
pub(crate) use path_anchor_index::{
    AnchorIndexEntry, PathAnchorIndex, build_path_anchor_index, load_path_anchor_index,
    store_path_anchor_index,
};
