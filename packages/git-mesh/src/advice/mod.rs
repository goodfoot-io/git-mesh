//! `git mesh advice` session store.
//!
//! Public re-exports for the advice subsystem.

pub mod candidates;
pub mod fingerprint;
pub mod render;
pub mod session;
pub mod workspace_tree;

pub use session::SessionStore;
pub use session::state::{BaselineState, LastFlushState, ReadRecord, TouchInterval};
pub use session::store::{LockGuard, LockTimeout};
pub use workspace_tree::{DiffEntry, WorkspaceTree, capture, diff_trees};

pub use candidates::{
    Candidate, CandidateInput, Density, MeshRange, MeshRangeStatus, ReasonKind,
    StagedAddr, StagingState,
    detect_delta_intersects_mesh, detect_partner_drift, detect_range_shrink,
    detect_read_intersects_mesh, detect_rename_consequence, detect_session_co_touch,
    detect_staging_cross_cut,
};
pub use fingerprint::fingerprint;

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
