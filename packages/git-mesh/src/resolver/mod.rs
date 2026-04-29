//! Resolver: compute staleness for anchors and meshes (§5).
//!
//! Layered HEAD/Index/Worktree resolution atop the HEAD-resolved
//! location; the staged-mesh layer surfaces `PendingFinding`s and
//! matches `acknowledged_by` by `anchor_id` (re-normalized on the sidecar
//! freshness stamp).
//!
//! Module map:
//!
//! - `walker` — anchor..HEAD history walk, hunk math.
//! - `layers` — index/worktree diff parsing, normalized reads,
//!   LFS + custom filter-process orchestration.
//! - `engine` — top-level `resolve_anchor` / `resolve_mesh` /
//!   `stale_meshes`, acknowledgment matching, the concurrency
//!   SHA-trailer guard.
//! - [`attribution`] — `culprit_commit` HEAD-source blame.

#![allow(dead_code)]

pub mod attribution;
pub(crate) mod engine;
pub(crate) mod layers;
pub(crate) mod session;
pub(crate) mod walker;

pub use attribution::culprit_commit;
pub use engine::pending::build_pending_findings;
pub use engine::{resolve_anchor, resolve_mesh, stale_meshes};
