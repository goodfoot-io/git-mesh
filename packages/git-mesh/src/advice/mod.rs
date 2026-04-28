//! `git mesh advice` session store.
//!
//! Public re-exports for the advice subsystem.

pub mod candidates;
pub mod debug;
pub mod detector;
pub mod fingerprint;
pub mod path_filter;
pub mod render;
pub mod session;
pub mod structured;
pub mod suggest;
pub mod suggestion;
pub mod workspace_tree;

pub use session::SessionStore;
pub use session::state::{BaselineState, LastFlushState, ReadRecord, TouchInterval};
pub use session::store::{LockGuard, LockTimeout};
pub use workspace_tree::{DiffEntry, LineRange, WorkspaceTree, capture, diff_trees};

pub use candidates::{
    Candidate, CandidateInput, DeltaIntersectsMeshDetector, Density, MeshAnchor, MeshAnchorStatus,
    PartnerDriftDetector, RangeShrinkDetector, ReadIntersectsMeshDetector, ReasonKind,
    RenameConsequenceDetector, StagedAddr, StagingCrossCutDetector, StagingState,
    candidate_to_suggestion, detect_delta_intersects_mesh, detect_partner_drift,
    detect_range_shrink, detect_read_intersects_mesh, detect_rename_consequence,
    detect_staging_cross_cut,
};
pub use detector::Detector;
pub use fingerprint::fingerprint;
pub use path_filter::is_acceptable_path;
pub use suggest::{SuggestConfig, SuggestDetector};
pub use suggestion::{ConfidenceBand, DriftMeta, ScoreBreakdown, Suggestion, Viability};

/// Re-exported submodules for test access.
pub mod state {
    pub use super::session::state::{BaselineState, LastFlushState, ReadRecord, TouchInterval};
}
pub mod store {
    pub use super::session::store::{
        LockGuard, LockTimeout, acquire_lock, advice_base_dir, append_jsonl_line, atomic_write,
        repo_key, session_dir,
    };
}
