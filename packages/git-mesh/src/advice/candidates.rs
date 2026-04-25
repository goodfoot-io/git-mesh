//! Candidate detectors for the advice subsystem.
//!
//! These pure functions operate on in-memory `CandidateInput` — no SQL, no
//! `rusqlite::Connection`. The existing SQL-driven render path in
//! `intersections.rs` is unchanged.
//!
//! ## Note on `detect_delta_intersects_mesh` hunk resolution
//!
//! `DiffEntry::Modified` carries only old/new OIDs and path, not hunk ranges.
//! Rather than shelling out for `git diff -U0`, this detector treats any
//! `Modified` entry on a meshed path as overlapping every range in that path
//! (over-emit, never under-emit). Sub-card C may tighten this with hunk
//! parsing if needed.

pub use crate::advice::intersections::{Candidate, Density, ReasonKind};
use crate::advice::session::state::{ReadRecord, TouchInterval};
use crate::advice::workspace_tree::DiffEntry;

// ── Pure-data types ──────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshRangeStatus {
    Stable,
    Changed,
    Moved,
    Terminal,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MeshRange {
    pub name: String,
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
    pub status: MeshRangeStatus,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct StagedAddr {
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
}

#[allow(dead_code)]
pub struct StagingState<'a> {
    pub adds: &'a [StagedAddr],
    pub removes: &'a [StagedAddr],
}

#[allow(dead_code)]
pub struct CandidateInput<'a> {
    pub session_delta: &'a [DiffEntry],
    pub incr_delta: &'a [DiffEntry],
    pub new_reads: &'a [ReadRecord],
    pub touch_intervals: &'a [TouchInterval],
    pub mesh_ranges: &'a [MeshRange],
    pub staging: StagingState<'a>,
}

// ── Detector stubs ───────────────────────────────────────────────────────────

/// Emit `Partner` for each `new_reads` interval that intersects a mesh range.
#[allow(dead_code)]
pub fn detect_read_intersects_mesh(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `Partner`/`WriteAcross` for `incr_delta` Modified/Deleted/Renamed
/// entries whose path appears in `mesh_ranges`.
#[allow(dead_code)]
pub fn detect_delta_intersects_mesh(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `Terminal` for `mesh_ranges` rows with CHANGED/MOVED/Terminal status.
#[allow(dead_code)]
pub fn detect_partner_drift(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `RenameLiteral` for `session_delta` Renamed entries whose old path is
/// meshed.
#[allow(dead_code)]
pub fn detect_rename_consequence(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `RangeCollapse` when blob-line-count comparison shows a meshed path
/// shrinking between `DiffEntry::Modified.old_oid` and `new_oid`.
#[allow(dead_code)]
pub fn detect_range_shrink(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `NewGroup` for co-touch pairs from `touch_intervals` that exceed
/// frequency thresholds, filtering generated/ignored/vendored/lockfile/binary.
#[allow(dead_code)]
pub fn detect_session_co_touch(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}

/// Emit `StagingCrossCut`/`EmptyMesh` for `staging.adds`/`staging.removes`
/// vs `mesh_ranges`.
#[allow(dead_code)]
pub fn detect_staging_cross_cut(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    // TODO Phase C
    Vec::new()
}
