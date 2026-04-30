//! git-mesh v2 library crate.
//!
//! Public API lives at the crate root via the curated re-exports below.
//! Modules are organized by concern:
//!
//! * [`types`]       — data shapes (spec §4).
//! * [`git`]         — git plumbing helpers.
//! * [`anchor`]       — Anchor blob create/read/parse/serialize (§4.1, §6.1).
//! * [`mesh`]        — mesh read/commit/structural (§6).
//! * [`staging`]     — `.git/mesh/staging/` area (§6.3, §6.4).
//! * [`file_index`]  — `.git/mesh/file-index` lookup table (§3.4).
//! * [`resolver`]    — layered staleness resolver (§5).
//! * [`sync`]        — fetch/push + lazy refspec (§7).
//! * [`validation`]  — name validation (§3.5, §10.2).
//! * [`cli`]         — clap surface; consumed by the binary.

pub mod advice;
pub mod anchor;
pub mod cli;
pub mod file_index;
pub mod git;
pub mod mesh;
pub mod perf;
pub mod resolver;
pub mod staging;
pub mod sync;
pub mod types;
pub mod validation;

pub use anchor::{anchor_ref_path, create_anchor, parse_anchor, read_anchor, serialize_anchor};
pub use file_index::{
    IndexEntry, ls_all, ls_by_path, ls_by_path_line_range, read_index, rebuild_index,
};
pub use git::read_git_text;
pub use mesh::{
    AnchorCompactOutcome, AnchorCompactRecord, MeshCommitInfo, MeshCompactOutcome, commit_mesh,
    compact_mesh, delete_mesh, is_ancestor_commit, list_mesh_names, mesh_commit_info,
    mesh_commit_info_at, mesh_log, read_mesh, read_mesh_at, rename_mesh, resolve_commit_ish,
    restore_mesh, revert_mesh, show_mesh, show_mesh_at,
};
pub use resolver::{culprit_commit, resolve_anchor, resolve_mesh, stale_meshes};
pub use staging::{
    StagedAdd, StagedConfig, StagedRemove, Staging, append_add, append_config, append_remove,
    clear_staging, list_staged_mesh_names, read_staging, set_why,
};
pub use sync::{default_remote, ensure_refspec_configured, fetch_mesh_refs, push_mesh_refs};
pub use types::*;
pub use validation::{RESERVED_MESH_NAMES, validate_anchor_id, validate_mesh_name};
