//! Shared drift-label formatter for `stale`, `stale --patch`, and `show`.
//!
//! Maps an anchor's resolved status to a human-readable label using the
//! seven-row vocabulary table from the card spec:
//!
//! | State | Label |
//! |---|---|
//! | Changed in worktree | `changed in the working tree` |
//! | Deleted in worktree | `deleted in the working tree` |
//! | Changed in index | `changed in the index` |
//! | Deleted in index | `deleted in the index` |
//! | Changed at sha | `changed in <sha>` |
//! | Orphaned at sha | `orphaned in <sha>` |
//! | Orphaned (no sha) | `orphaned` |
//!
//! Precedence is enforced by the engine before reaching this formatter:
//! worktree → index → HEAD history walk.

use crate::types::{AnchorStatus, Culprit, DriftSource};

/// Format a human-readable drift label for a single anchor.
///
/// # Arguments
///
/// * `status` — The resolved anchor status.
/// * `source` — The layer at which drift was detected (`None` for `Fresh`
///   and terminal statuses).
/// * `culprit` — The commit blamed for HEAD-layer drift (`None` when
///   `source != Head` or when the anchor sha is unreachable).
/// * `current_blob_present` — `true` when the content still exists at the
///   drift locus (path present); `false` when the path has been removed
///   (deletion / orphan via rename).
#[allow(unused_variables)]
pub fn format_drift_label(
    status: &AnchorStatus,
    source: Option<DriftSource>,
    culprit: Option<&Culprit>,
    current_blob_present: bool,
) -> String {
    todo!("Phase 3: implement drift label formatter")
}
