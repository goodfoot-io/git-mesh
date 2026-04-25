//! Workspace-tree capture and diff helpers.

use std::path::PathBuf;

use anyhow::Result;

/// A single change between two tree objects.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DiffEntry {
    /// File content changed.
    Modified { path: String },
    /// File was added.
    Added { path: String },
    /// File was deleted.
    Deleted { path: String },
    /// File was renamed (with optional similarity score).
    Renamed { from: String, to: String, score: u8 },
    /// File mode changed (e.g. exec bit toggled).
    ModeChange { path: String },
}

/// A workspace tree snapshot backed by a temp Git object directory.
#[allow(dead_code)]
pub struct WorkspaceTree {
    /// SHA-1 hex of the tree object.
    pub tree_sha: String,
    /// Directory holding the temporary Git objects for this tree.
    pub objects_dir: PathBuf,
}

/// Capture the current workspace state into `objects_dir`, returning a
/// `WorkspaceTree`. Uses `GIT_INDEX_FILE` / `GIT_OBJECT_DIRECTORY` overrides
/// so the real index is not mutated.
#[allow(dead_code)]
pub fn capture(_repo: &gix::Repository, _objects_dir: &std::path::Path) -> Result<WorkspaceTree> {
    unimplemented!()
}

/// Compute the diff between two tree SHAs, merging alternate object stores.
#[allow(dead_code)]
pub fn diff_trees(
    _repo: &gix::Repository,
    _from_sha: &str,
    _to_sha: &str,
    _from_objects: &std::path::Path,
    _to_objects: &std::path::Path,
) -> Result<Vec<DiffEntry>> {
    unimplemented!()
}
