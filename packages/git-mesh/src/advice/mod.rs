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
pub use session::state::{ReadRecord, TouchInterval, TouchKind};
pub use session::store::{LockGuard, LockTimeout};
pub use workspace_tree::{DiffEntry, LineRange, WorkspaceTree, capture, diff_trees};

pub use candidates::{
    Candidate, CandidateInput, Density, MeshAnchor, MeshAnchorStatus, ReasonKind, StagedAddr,
    StagingState, candidate_to_suggestion,
};
pub use detector::Detector;
pub use fingerprint::fingerprint;
pub use path_filter::is_acceptable_path;
pub use suggest::{SuggestConfig, SuggestDetector};
pub use suggestion::{ConfidenceBand, DriftMeta, ScoreBreakdown, Suggestion, Viability};

/// Re-exported submodules for test access.
pub mod state {
    pub use super::session::state::{ReadRecord, TouchInterval, TouchKind};
}
pub mod store {
    pub use super::session::store::{
        LockGuard, LockTimeout, acquire_lock, advice_base_dir, append_jsonl_line, atomic_write,
        repo_key, session_dir,
    };
}
