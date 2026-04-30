//! Candidate types for the advice subsystem.
//!
//! The legacy per-tool drift detectors that lived here have been removed: the
//! resolver-based flush/read pipeline (`cli::advice`) and the n-ary suggester
//! (`advice::suggest`) are the only intended advice surfaces. The types in
//! this module remain — they are still consumed by the suggester pipeline and
//! by `candidate_to_suggestion`, which converts a `Candidate` into a
//! `Suggestion` for renderers.

use crate::advice::workspace_tree::DiffEntry;

// ── Candidate types (preserved from former `intersections.rs`) ───────────────

/// Density ladder — §12.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    /// Partner list only.
    L0,
    /// Partner list + one excerpt.
    L1,
    /// Partner list + excerpt + ready-to-run command.
    L2,
}

/// Reason-kind: matches the T1…T11 message-type inventory. Used as a
/// stable dedup key and as the key for per-reason doc topics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReasonKind {
    /// T1 partner list.
    Partner,
    /// T2 partner excerpt on write.
    WriteAcross,
    /// T3 rename literal in partner.
    RenameLiteral,
    /// T4 anchor collapse on partner.
    RangeCollapse,
    /// T5 losing coherence.
    LosingCoherence,
    /// T6 symbol rename hits in partner.
    SymbolRename,
    /// T7 new-mesh candidate.
    NewMesh,
    /// T8 staging cross-cut.
    StagingCrossCut,
    /// T9 empty-mesh risk.
    EmptyMesh,
    /// T10 pending-commit re-anchor.
    PendingCommit,
    /// T11 terminal status.
    Terminal,
}

impl std::fmt::Display for ReasonKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ReasonKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonKind::Partner => "partner",
            ReasonKind::WriteAcross => "write_across",
            ReasonKind::RenameLiteral => "rename_literal",
            ReasonKind::RangeCollapse => "range_collapse",
            ReasonKind::LosingCoherence => "losing_coherence",
            ReasonKind::SymbolRename => "symbol_rename",
            ReasonKind::NewMesh => "new_mesh",
            ReasonKind::StagingCrossCut => "staging_cross_cut",
            ReasonKind::EmptyMesh => "empty_mesh",
            ReasonKind::PendingCommit => "pending_commit",
            ReasonKind::Terminal => "terminal",
        }
    }

    pub fn doc_topic(self) -> Option<&'static str> {
        match self {
            ReasonKind::Partner => None, // L0 — no topic
            ReasonKind::WriteAcross => Some("editing-across-files"),
            ReasonKind::RenameLiteral => Some("renames"),
            ReasonKind::RangeCollapse => Some("shrinking-ranges"),
            ReasonKind::LosingCoherence => Some("narrow-or-retire"),
            ReasonKind::SymbolRename => Some("exported-symbols"),
            ReasonKind::NewMesh => Some("recording-a-mesh"),
            ReasonKind::StagingCrossCut => Some("cross-mesh-overlap"),
            ReasonKind::EmptyMesh => Some("empty-meshes"),
            ReasonKind::PendingCommit => None, // L0 — no topic
            ReasonKind::Terminal => Some("terminal-states"),
        }
    }

    pub fn default_density(self) -> Density {
        match self {
            ReasonKind::Partner | ReasonKind::PendingCommit | ReasonKind::Terminal => Density::L0,
            ReasonKind::WriteAcross => Density::L1,
            _ => Density::L2,
        }
    }
}

/// A surfacing candidate — one row per (mesh, reason, partner, trigger).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub mesh: String,
    pub mesh_why: String,
    pub reason_kind: ReasonKind,
    pub partner_path: String,
    pub partner_start: Option<i64>,
    pub partner_end: Option<i64>,
    /// The file the developer just touched (trigger anchor) — only used for
    /// dedup and for the command text. May be empty.
    pub trigger_path: String,
    pub trigger_start: Option<i64>,
    pub trigger_end: Option<i64>,
    /// The recorded mesh anchor on the trigger side — the anchor the
    /// developer's edit hit. Surfaced in the bullet list so the mesh's
    /// complete anchor set is visible. Empty path = unknown / not
    /// applicable (e.g. cross-cutting candidates).
    pub touched_path: String,
    pub touched_start: Option<i64>,
    pub touched_end: Option<i64>,
    /// Bracket marker appended to the partner line (CHANGED, STAGED, …).
    /// Empty = no marker.
    pub partner_marker: String,
    /// Prose clause after an em-dash on the partner line. Empty = none.
    pub partner_clause: String,
    pub density: Density,
    /// Optional ready-to-run command (L2). Empty for L0/L1.
    pub command: String,
    /// L1/L2 excerpt block attached to a specific partner path+anchor. Empty
    /// for L0.
    pub excerpt_of_path: String,
    pub excerpt_start: Option<i64>,
    pub excerpt_end: Option<i64>,
    /// Old blob OID (SHA) for this diff entry. None when not available.
    pub old_blob: Option<String>,
    /// New blob OID (SHA) for this diff entry. None when not available.
    pub new_blob: Option<String>,
    /// Old path before a rename. None when not a rename or not available.
    pub old_path: Option<String>,
    /// New path after a rename. None when not a rename or not available.
    pub new_path: Option<String>,
}

// ── Pure-data types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshAnchorStatus {
    Stable,
    Changed,
    Moved,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct MeshAnchor {
    pub name: String,
    pub why: String,
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
    /// True when the mesh pin covers the whole file rather than a line range.
    pub whole: bool,
    pub status: MeshAnchorStatus,
}

#[derive(Debug, Clone)]
pub struct StagedAddr {
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
    /// True when the staged address covers the whole file.
    pub whole: bool,
}

pub struct StagingState<'a> {
    pub adds: &'a [StagedAddr],
    pub removes: &'a [StagedAddr],
}

pub struct CandidateInput<'a> {
    pub session_delta: &'a [DiffEntry],
    pub incr_delta: &'a [DiffEntry],
    pub new_reads: &'a [crate::advice::session::state::ReadRecord],
    pub touch_intervals: &'a [crate::advice::session::state::TouchInterval],
    pub mesh_anchors: &'a [MeshAnchor],
    pub internal_path_prefixes: &'a [String],
    pub staging: StagingState<'a>,
}

// ── Candidate → Suggestion conversion ────────────────────────────────────────

/// Convert a `Candidate` produced by a pairwise drift detector into a
/// `Suggestion` at the `Detector` seam.
///
/// Layout:
/// - `participants[0]` — partner anchor (always present)
/// - `participants[1]` — trigger anchor (omitted when `trigger_path` is empty,
///   e.g. partner-drift candidates)
///
/// Extra rendering fields (marker, clause, density, command, excerpt, touched)
/// are carried in `Suggestion.meta` (a typed `DriftMeta`), not encoded
/// as a JSON string in `label`. The renderer dispatches on `s.meta.is_some()`.
///
/// `ConfidenceBand::High` is used for all drift detectors — they are
/// deterministic, single-channel signals with no probabilistic scoring.
pub fn candidate_to_suggestion(c: &Candidate) -> crate::advice::suggestion::Suggestion {
    use crate::advice::suggestion::{ConfidenceBand, ScoreBreakdown, Suggestion, Viability};

    let partner = MeshAnchor {
        name: c.mesh.clone(),
        why: c.mesh_why.clone(),
        path: std::path::PathBuf::from(&c.partner_path),
        start: c.partner_start.map(|v| v as u32).unwrap_or(0),
        end: c.partner_end.map(|v| v as u32).unwrap_or(u32::MAX),
        whole: c.partner_start.is_none() || c.partner_end.is_none(),
        status: MeshAnchorStatus::Stable,
    };

    let mut participants = vec![partner];

    if !c.trigger_path.is_empty() {
        let trigger = MeshAnchor {
            name: c.mesh.clone(),
            why: c.mesh_why.clone(),
            path: std::path::PathBuf::from(&c.trigger_path),
            start: c.trigger_start.map(|v| v as u32).unwrap_or(0),
            end: c.trigger_end.map(|v| v as u32).unwrap_or(u32::MAX),
            whole: c.trigger_start.is_none() || c.trigger_end.is_none(),
            status: MeshAnchorStatus::Stable,
        };
        participants.push(trigger);
    }

    use crate::advice::suggestion::DriftMeta;

    let meta = DriftMeta {
        reason_kind: c.reason_kind.as_str().to_string(),
        partner_marker: c.partner_marker.clone(),
        partner_clause: c.partner_clause.clone(),
        density: match c.density {
            Density::L0 => 0,
            Density::L1 => 1,
            Density::L2 => 2,
        },
        command: c.command.clone(),
        touched_path: c.touched_path.clone(),
        touched_start: c.touched_start,
        touched_end: c.touched_end,
        excerpt_of_path: c.excerpt_of_path.clone(),
        excerpt_start: c.excerpt_start,
        excerpt_end: c.excerpt_end,
    };

    // Use the human-readable label (empty string — drift suggestions don't
    // need a display label; the renderer reads everything from `meta`).
    Suggestion::new_drift(
        ConfidenceBand::High,
        Viability::Ready,
        ScoreBreakdown {
            shared_id: 0.0,
            co_edit: 0.0,
            trigram: 0.0,
            composite: 1.0,
        },
        participants,
        String::new(),
        meta,
    )
}
