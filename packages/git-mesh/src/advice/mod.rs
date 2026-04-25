//! `git mesh advice` session store.
//!
//! Public re-exports for the advice subsystem.

pub mod audit;
pub mod candidates;
pub mod fingerprint;
pub mod db;
pub mod events;
pub mod flush;
pub mod intersections;
pub mod render;
pub mod session;
pub mod workspace_tree;

pub use db::{open_store, sanitize_session_id};
pub use events::{
    AuditRecord, CONTENT_BYTE_CAP, append_commit, append_read, append_snapshot, append_write,
};
pub use flush::run_flush;
pub use session::SessionStore;
pub use session::state::{BaselineState, LastFlushState, ReadRecord, TouchInterval};
pub use session::store::{LockGuard, LockTimeout};
pub use workspace_tree::{DiffEntry, WorkspaceTree, capture, diff_trees};

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
