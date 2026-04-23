//! Mesh read/write operations. See §6.
//!
//! Split by concern:
//! - [`commit`] — staging resolution + mesh commit pipeline (§6.1, §6.2).
//! - [`read`]   — read-only views (§6.5, §6.6, §10.4).
//! - [`structural`] — delete, mv, restore, revert (§6.8).

pub mod commit;
pub mod read;
pub mod structural;

pub use commit::commit_mesh;
pub use read::{
    is_ancestor_commit, list_mesh_names, mesh_commit_info, mesh_commit_info_at, mesh_log,
    read_mesh, read_mesh_at, resolve_commit_ish, show_mesh, show_mesh_at, MeshCommitInfo,
};
pub use structural::{delete_mesh, rename_mesh, restore_mesh, revert_mesh};
