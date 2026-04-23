//! git-mesh v2 library crate.
//!
//! Public API lives at the crate root via the curated re-exports below.
//! Modules are organized by concern:
//!
//! * [`types`]       — data shapes (spec §4).
//! * [`git`]         — git plumbing helpers.
//! * [`range`]       — Range blob create/read/parse/serialize (§4.1, §6.1).
//! * [`mesh`]        — mesh read/commit/structural (§6).
//! * [`staging`]     — `.git/mesh/staging/` area (§6.3, §6.4).
//! * [`file_index`]  — `.git/mesh/file-index` lookup table (§3.4).
//! * [`stale`]       — resolver (§5).
//! * [`sync`]        — fetch/push + lazy refspec (§7).
//! * [`validation`]  — name validation (§3.5, §10.2).
//! * [`cli`]         — clap surface; consumed by the binary.

pub mod cli;
pub mod file_index;
pub mod git;
pub mod mesh;
pub mod range;
pub mod stale;
pub mod staging;
pub mod sync;
pub mod types;
pub mod validation;

pub use git::read_git_text;
pub use range::{create_range, parse_range, range_ref_path, read_range, serialize_range};
pub use mesh::{
    commit_mesh, delete_mesh, is_ancestor_commit, list_mesh_names, mesh_commit_info,
    mesh_commit_info_at, mesh_log, read_mesh, read_mesh_at, rename_mesh, resolve_commit_ish,
    restore_mesh, revert_mesh, show_mesh, show_mesh_at, MeshCommitInfo,
};
pub use staging::{
    append_add, append_config, append_remove, clear_staging, drift_check, read_staging,
    set_message, status_view, DriftFinding, StagedAdd, StagedConfig, StagedRemove, Staging,
    StatusView,
};
pub use file_index::{
    ls_all, ls_by_path, ls_by_path_range, read_index, rebuild_index, IndexEntry,
};
pub use stale::{culprit_commit, resolve_mesh, resolve_range, stale_meshes};
pub use sync::{default_remote, ensure_refspec_configured, fetch_mesh_refs, push_mesh_refs};
pub use types::*;
pub use validation::{validate_mesh_name, validate_range_id, RESERVED_MESH_NAMES};
