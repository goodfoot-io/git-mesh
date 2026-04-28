//! Candidate detectors for the advice subsystem.
//!
//! These pure functions operate on in-memory `CandidateInput` — no SQL, no
//! `rusqlite::Connection`. The existing SQL-driven render path in
//! `intersections.rs` is unchanged.
//!
//! ## Deferred detectors
//!
//! `detect_range_shrink` returns an empty vec until sub-card C extends
//! `DiffEntry` with blob line counts. Over-emitting without that data would
//! burn fingerprints in the dedupe set before correct candidates are available
//! (not recoverable without manual reset).

use crate::advice::session::state::{ReadRecord, TouchInterval};
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
    /// T4 range collapse on partner.
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
    /// The file the developer just touched (trigger range) — only used for
    /// dedup and for the command text. May be empty.
    pub trigger_path: String,
    pub trigger_start: Option<i64>,
    pub trigger_end: Option<i64>,
    /// The recorded mesh range on the trigger side — the range the
    /// developer's edit hit. Surfaced in the bullet list so the mesh's
    /// complete range set is visible. Empty path = unknown / not
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
    /// L1/L2 excerpt block attached to a specific partner path+range. Empty
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
pub enum MeshRangeStatus {
    Stable,
    Changed,
    Moved,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct MeshRange {
    pub name: String,
    pub why: String,
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
    /// True when the mesh pin covers the whole file rather than a line range.
    pub whole: bool,
    pub status: MeshRangeStatus,
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
    pub new_reads: &'a [ReadRecord],
    pub touch_intervals: &'a [TouchInterval],
    pub mesh_ranges: &'a [MeshRange],
    pub internal_path_prefixes: &'a [String],
    pub staging: StagingState<'a>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns true if `[a_start, a_end]` overlaps `[b_start, b_end]` (inclusive).
fn ranges_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start <= b_end && b_start <= a_end
}

fn bare_candidate(
    mesh: &str,
    mesh_why: &str,
    kind: ReasonKind,
    partner: (&str, Option<i64>, Option<i64>),
    trigger: (&str, Option<i64>, Option<i64>),
) -> Candidate {
    let (partner_path, partner_start, partner_end) = partner;
    let (trigger_path, trigger_start, trigger_end) = trigger;
    Candidate {
        mesh: mesh.to_string(),
        mesh_why: mesh_why.to_string(),
        reason_kind: kind,
        partner_path: partner_path.to_string(),
        partner_start,
        partner_end,
        trigger_path: trigger_path.to_string(),
        trigger_start,
        trigger_end,
        touched_path: String::new(),
        touched_start: None,
        touched_end: None,
        partner_marker: String::new(),
        partner_clause: String::new(),
        density: kind.default_density(),
        command: String::new(),
        excerpt_of_path: String::new(),
        excerpt_start: None,
        excerpt_end: None,
        old_blob: None,
        new_blob: None,
        old_path: None,
        new_path: None,
    }
}

fn same_range(a: &MeshRange, b: &MeshRange) -> bool {
    a.name == b.name && a.path == b.path && a.start == b.start && a.end == b.end
}

fn path_is_internal(path: &str, internal_path_prefixes: &[String]) -> bool {
    internal_path_prefixes.iter().any(|prefix| {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn partner_candidates_for_trigger(
    input: &CandidateInput<'_>,
    trigger_path: &str,
    trigger_start: Option<i64>,
    trigger_end: Option<i64>,
    touched: &MeshRange,
) -> Vec<Candidate> {
    partner_candidates_for_trigger_kind(
        input,
        ReasonKind::Partner,
        trigger_path,
        trigger_start,
        trigger_end,
        touched,
    )
}

fn partner_candidates_for_trigger_kind(
    input: &CandidateInput<'_>,
    kind: ReasonKind,
    trigger_path: &str,
    trigger_start: Option<i64>,
    trigger_end: Option<i64>,
    touched: &MeshRange,
) -> Vec<Candidate> {
    input
        .mesh_ranges
        .iter()
        .filter(|partner| partner.name == touched.name && !same_range(partner, touched))
        .map(|partner| {
            let partner_path = partner.path.to_string_lossy();
            let (ps, pe) = if partner.whole {
                (None, None)
            } else {
                (Some(partner.start as i64), Some(partner.end as i64))
            };
            let mut c = bare_candidate(
                &partner.name,
                &partner.why,
                kind,
                (&partner_path, ps, pe),
                (trigger_path, trigger_start, trigger_end),
            );
            c.touched_path = touched.path.to_string_lossy().to_string();
            if !touched.whole {
                c.touched_start = Some(touched.start as i64);
                c.touched_end = Some(touched.end as i64);
            }
            c
        })
        .collect()
}

// ── Detector functions ───────────────────────────────────────────────────────

/// Emit `Partner` for each `new_reads` interval that intersects a mesh range.
pub fn detect_read_intersects_mesh(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for read in input.new_reads {
        if path_is_internal(&read.path, input.internal_path_prefixes) {
            continue;
        }
        let read_start = read.start_line.unwrap_or(0);
        let read_end = read.end_line.unwrap_or(u32::MAX);
        for range in input.mesh_ranges {
            let path_str = range.path.to_string_lossy();
            if read.path != path_str.as_ref() {
                continue;
            }
            let mesh_start = if range.whole { 0 } else { range.start };
            let mesh_end = if range.whole { u32::MAX } else { range.end };
            if ranges_overlap(read_start, read_end, mesh_start, mesh_end) {
                let before = out.len();
                out.extend(partner_candidates_for_trigger(
                    input,
                    &read.path,
                    read.start_line.map(i64::from),
                    read.end_line.map(i64::from),
                    range,
                ));
                for c in &out[before..] {
                    crate::advice_debug!(
                        "detect_read_intersects_mesh",
                        "mesh" => c.mesh,
                        "reason_kind" => c.reason_kind,
                        "partner" => format!("{}#L{}-L{}", c.partner_path,
                            c.partner_start.unwrap_or(0), c.partner_end.unwrap_or(0)),
                        "trigger" => format!("{}#L{}-L{}", c.trigger_path,
                            c.trigger_start.unwrap_or(0), c.trigger_end.unwrap_or(0)),
                        "marker" => if c.partner_marker.is_empty() { "-" } else { &c.partner_marker }
                    );
                }
            }
        }
    }
    out
}

/// Conservative whole-file fallback for incremental deltas.
///
/// Without hunk ranges, a changed path can only be treated as whole-file
/// attention. To fail closed, this emits only partners already present in the
/// same mesh as an existing range on the changed/deleted/old/new path.
pub fn detect_delta_intersects_mesh(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in input.incr_delta {
        match entry {
            DiffEntry::Modified { path, old_oid, new_oid, hunks }
            | DiffEntry::ModeChange { path, old_oid, new_oid, hunks } => {
                let mut cands = delta_path_partners(input, path, hunks.as_deref());
                for c in &mut cands {
                    c.old_blob = old_oid.clone();
                    c.new_blob = new_oid.clone();
                }
                out.extend(cands);
            }
            DiffEntry::Added { path, new_oid, hunks } => {
                let mut cands = delta_path_partners(input, path, hunks.as_deref());
                for c in &mut cands {
                    c.new_blob = new_oid.clone();
                }
                out.extend(cands);
            }
            DiffEntry::Deleted { path, old_oid } => {
                let mut cands = delta_path_partners(input, path, None);
                for c in &mut cands {
                    c.old_blob = old_oid.clone();
                }
                out.extend(cands);
            }
            DiffEntry::Renamed { from, to, new_oid, hunks, .. } => {
                // When `from` is a meshed path, detect_rename_consequence
                // will emit an L2 candidate covering the re-record; skip
                // producing partner candidates here to avoid duplicating
                // partners and surfacing the old (non-existent) path as a
                // trigger.
                //
                // When `from` is NOT a meshed path but `to` is (a file was
                // renamed *into* a meshed location), treat it like an Added.
                let from_is_meshed = input
                    .mesh_ranges
                    .iter()
                    .any(|r| r.path.to_string_lossy() == from.as_str());
                if !from_is_meshed {
                    let mut cands_to = delta_path_partners(input, to, hunks.as_deref());
                    for c in &mut cands_to {
                        c.new_blob = new_oid.clone();
                    }
                    out.extend(cands_to);
                }
            }
        }
    }
    out
}

/// Build a map of `old_path → new_path` for all renames in `session_delta`.
fn session_rename_map(input: &CandidateInput<'_>) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for entry in input.session_delta {
        if let DiffEntry::Renamed { from, to, .. } = entry {
            m.insert(from.clone(), to.clone());
        }
    }
    m
}

fn delta_path_partners(
    input: &CandidateInput<'_>,
    path: &str,
    hunks: Option<&[crate::advice::workspace_tree::LineRange]>,
) -> Vec<Candidate> {
    delta_path_partners_inner(input, path, hunks, &session_rename_map(input))
}

/// Returns true if any hunk's `[start, end]` overlaps the line-bounded mesh
/// range `[range.start, range.end]` (inclusive on both ends). When `hunks` is
/// `None` or empty the function returns `true` — the no-false-negative
/// invariant means "unknown hunks" must always fire.
fn hunks_overlap_range(
    range: &MeshRange,
    hunks: Option<&[crate::advice::workspace_tree::LineRange]>,
) -> bool {
    match hunks {
        None | Some([]) => true,
        Some(hs) => hs
            .iter()
            .any(|h| ranges_overlap(h.start, h.end, range.start, range.end)),
    }
}

fn delta_path_partners_inner(
    input: &CandidateInput<'_>,
    path: &str,
    hunks: Option<&[crate::advice::workspace_tree::LineRange]>,
    rename_map: &std::collections::HashMap<String, String>,
) -> Vec<Candidate> {
    if path_is_internal(path, input.internal_path_prefixes) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for range in input.mesh_ranges {
        let range_path = range.path.to_string_lossy();
        if path != range_path.as_ref() {
            continue;
        }
        // Line-range filter: a line-bounded mesh range only fires when at
        // least one known hunk overlaps it. Whole-file ranges always fire,
        // and unknown hunks (`None` / empty) always fire (no false negatives).
        if !range.whole && !hunks_overlap_range(range, hunks) {
            continue;
        }
        // Emit one candidate per partner range in this mesh.
        for partner in input.mesh_ranges {
            if partner.name != range.name || same_range(partner, range) {
                continue;
            }
            let partner_path_str = partner.path.to_string_lossy().to_string();
            let (ps, pe) = if partner.whole {
                (None, None)
            } else {
                (Some(partner.start as i64), Some(partner.end as i64))
            };
            // If the partner range's path was renamed in the session, surface
            // the new path instead of the old (non-existent) path, and annotate
            // with [RENAMED] / was <old>.
            let (effective_partner_path, marker, clause) =
                if let Some(new_path) = rename_map.get(&partner_path_str) {
                    (
                        new_path.clone(),
                        "[RENAMED]".to_string(),
                        format!("was {partner_path_str}"),
                    )
                } else {
                    (partner_path_str.clone(), String::new(), String::new())
                };
            let mut c = bare_candidate(
                &partner.name,
                &partner.why,
                ReasonKind::Partner,
                (&effective_partner_path, ps, pe),
                (path, None, None),
            );
            c.partner_marker = marker;
            c.partner_clause = clause;
            c.touched_path = range_path.to_string();
            if !range.whole {
                c.touched_start = Some(range.start as i64);
                c.touched_end = Some(range.end as i64);
            }
            if effective_partner_path != partner_path_str {
                c.old_path = Some(partner_path_str);
                c.new_path = Some(effective_partner_path);
            }
            crate::advice_debug!(
                "detect_delta_intersects_mesh",
                "mesh" => c.mesh,
                "reason_kind" => c.reason_kind,
                "partner" => format!("{}#L{}-L{}", c.partner_path,
                    c.partner_start.unwrap_or(0), c.partner_end.unwrap_or(0)),
                "trigger" => c.trigger_path.clone(),
                "marker" => if c.partner_marker.is_empty() { "-" } else { &c.partner_marker }
            );
            out.push(c);
        }
    }
    out
}


/// Emit `Terminal` for `mesh_ranges` rows with Changed/Moved/Terminal status.
pub fn detect_partner_drift(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let touched_paths: std::collections::HashSet<&str> = input
        .incr_delta
        .iter()
        .chain(input.session_delta.iter())
        .flat_map(|entry| match entry {
            DiffEntry::Modified { path, .. }
            | DiffEntry::Added { path, .. }
            | DiffEntry::Deleted { path, .. }
            | DiffEntry::ModeChange { path, .. } => vec![path.as_str()],
            DiffEntry::Renamed { from, to, .. } => vec![from.as_str(), to.as_str()],
        })
        .collect();
    let mut out = Vec::new();
    for range in input.mesh_ranges {
        if matches!(
            range.status,
            MeshRangeStatus::Changed | MeshRangeStatus::Moved | MeshRangeStatus::Terminal
        ) {
            let path_str = range.path.to_string_lossy();
            if touched_paths.contains(path_str.as_ref()) {
                continue;
            }
            let (rs, re) = if range.whole {
                (None, None)
            } else {
                (Some(range.start as i64), Some(range.end as i64))
            };
            let marker = match range.status {
                MeshRangeStatus::Changed => "[CHANGED]",
                MeshRangeStatus::Moved => "[MOVED]",
                MeshRangeStatus::Terminal => "[TERMINAL]",
                MeshRangeStatus::Stable => "",
            };
            let mut c = bare_candidate(
                &range.name,
                &range.why,
                ReasonKind::Terminal,
                (&path_str, rs, re),
                ("", None, None),
            );
            c.partner_marker = marker.to_string();
            crate::advice_debug!(
                "detect_partner_drift",
                "mesh" => c.mesh,
                "reason_kind" => c.reason_kind,
                "partner" => format!("{}#L{}-L{}", c.partner_path,
                    c.partner_start.unwrap_or(0), c.partner_end.unwrap_or(0)),
                "marker" => marker
            );
            out.push(c);
        }
    }
    out
}

/// Emit `RenameLiteral` (L2) for `session_delta` Renamed entries whose old path
/// is a meshed range, with a ready-to-run `git mesh rm/add/commit` command.
///
/// When the renamed path (`from`) is uniquely determined (exactly one Renamed
/// entry maps it to a single `to`), emit a single L2 candidate carrying the
/// re-record command sequence. The trigger is the new path (`to`) — the old
/// path no longer exists on disk. The `partner_path` is also `to` so that the
/// bullet renders as an actionable address.
pub fn detect_rename_consequence(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in input.session_delta {
        if let DiffEntry::Renamed { from, to, .. } = entry {
            for range in input.mesh_ranges {
                let range_path = range.path.to_string_lossy();
                if from.as_str() != range_path.as_ref() {
                    continue;
                }
                // `from` is a meshed path. Emit a single L2 candidate with
                // the rm/add/commit command. Use `to` as both trigger and
                // partner so the rendered address is navigable.
                let mesh = &range.name;
                let command = format!(
                    "git mesh rm  {mesh} {from}\ngit mesh add {mesh} {to}\ngit mesh commit {mesh}"
                );
                let mut c = bare_candidate(
                    mesh,
                    &range.why,
                    ReasonKind::RenameLiteral,
                    (to, None, None),
                    (to, None, None),
                );
                c.density = Density::L2;
                c.command = command;
                c.old_path = Some(from.clone());
                c.new_path = Some(to.clone());
                crate::advice_debug!(
                    "detect_rename_consequence",
                    "mesh" => c.mesh,
                    "reason_kind" => c.reason_kind,
                    "from" => from,
                    "to" => to
                );
                out.push(c);
            }
        }
    }
    out
}

/// Deferred detector — currently returns no candidates.
///
/// User-experienced gap: range-collapse advice will not surface. A user
/// whose edit shrinks a meshed range below its recorded extent will not
/// be prompted to narrow-or-retire that range until this detector lands.
///
/// Why deferred: requires blob line counts on `DiffEntry`. Emitting
/// `RangeCollapse` on every modified meshed path would pollute the
/// fingerprint set; once correct collapses arrive their fingerprints
/// would already be burned. Tracked in card `main-1-2-4` (`Advice delta
/// redesign — Sub-card D`); do not enable until `DiffEntry` carries
/// old/new line counts.
pub fn detect_range_shrink(_input: &CandidateInput<'_>) -> Vec<Candidate> {
    Vec::new()
}

// Note: the pairwise `detect_session_co_touch` / `SessionCoTouchDetector` channel
// was removed in card main-13 slice 2. New-mesh recommendations are now produced
// n-ary and line-bounded by the `advice::suggest::run_suggest_pipeline` and
// folded into the user-facing render in `cli::advice::run_advice_render`.
// `ReasonKind::NewMesh` carries the n-ary mesh recommendations (card main-13
// outcomes 3/5).

/// Emit `StagingCrossCut`/`EmptyMesh` for `staging.adds`/`staging.removes`
/// vs `mesh_ranges`.
pub fn detect_staging_cross_cut(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();

    // staged adds overlapping a mesh range → StagingCrossCut
    for add in input.staging.adds {
        let add_path = add.path.to_string_lossy();
        let add_start = if add.whole { 0 } else { add.start };
        let add_end = if add.whole { u32::MAX } else { add.end };
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if add_path.as_ref() != range_path.as_ref() {
                continue;
            }
            let mesh_start = if range.whole { 0 } else { range.start };
            let mesh_end = if range.whole { u32::MAX } else { range.end };
            if ranges_overlap(add_start, add_end, mesh_start, mesh_end) {
                let (rs, re) = if range.whole {
                    (None, None)
                } else {
                    (Some(range.start as i64), Some(range.end as i64))
                };
                let (as_, ae) = if add.whole {
                    (None, None)
                } else {
                    (Some(add.start as i64), Some(add.end as i64))
                };
                crate::advice_debug!(
                    "detect_staging_cross_cut",
                    "mesh" => range.name,
                    "reason_kind" => ReasonKind::StagingCrossCut,
                    "partner" => format!("{}#L{}-L{}", range_path,
                        rs.unwrap_or(0), re.unwrap_or(0)),
                    "trigger" => format!("{}#L{}-L{}", add_path,
                        as_.unwrap_or(0), ae.unwrap_or(0))
                );
                out.push(bare_candidate(
                    &range.name,
                    &range.why,
                    ReasonKind::StagingCrossCut,
                    (&range_path, rs, re),
                    (&add_path, as_, ae),
                ));
            }
        }
    }

    // staged removes fully covering a mesh range → EmptyMesh
    for remove in input.staging.removes {
        let rem_path = remove.path.to_string_lossy();
        let rem_start = if remove.whole { 0 } else { remove.start };
        let rem_end = if remove.whole { u32::MAX } else { remove.end };
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if rem_path.as_ref() != range_path.as_ref() {
                continue;
            }
            let mesh_start = if range.whole { 0 } else { range.start };
            let mesh_end = if range.whole { u32::MAX } else { range.end };
            // A remove empties the mesh if it covers the entire mesh range
            if rem_start <= mesh_start && rem_end >= mesh_end {
                let (rs, re) = if range.whole {
                    (None, None)
                } else {
                    (Some(range.start as i64), Some(range.end as i64))
                };
                let (rms, rme) = if remove.whole {
                    (None, None)
                } else {
                    (Some(remove.start as i64), Some(remove.end as i64))
                };
                crate::advice_debug!(
                    "detect_staging_cross_cut",
                    "mesh" => range.name,
                    "reason_kind" => ReasonKind::EmptyMesh,
                    "partner" => format!("{}#L{}-L{}", range_path,
                        rs.unwrap_or(0), re.unwrap_or(0)),
                    "trigger" => format!("{}#L{}-L{}", rem_path,
                        rms.unwrap_or(0), rme.unwrap_or(0))
                );
                out.push(bare_candidate(
                    &range.name,
                    &range.why,
                    ReasonKind::EmptyMesh,
                    (&range_path, rs, re),
                    (&rem_path, rms, rme),
                ));
            }
        }
    }

    out
}

// ── Detector trait impls ─────────────────────────────────────────────────────

/// Convert a `Candidate` produced by a pairwise drift detector into a
/// `Suggestion` at the `Detector` seam.
///
/// Layout:
/// - `participants[0]` — partner range (always present)
/// - `participants[1]` — trigger range (omitted when `trigger_path` is empty,
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

    let partner = MeshRange {
        name: c.mesh.clone(),
        why: c.mesh_why.clone(),
        path: std::path::PathBuf::from(&c.partner_path),
        start: c.partner_start.map(|v| v as u32).unwrap_or(0),
        end: c.partner_end.map(|v| v as u32).unwrap_or(u32::MAX),
        whole: c.partner_start.is_none() || c.partner_end.is_none(),
        status: MeshRangeStatus::Stable,
    };

    let mut participants = vec![partner];

    if !c.trigger_path.is_empty() {
        let trigger = MeshRange {
            name: c.mesh.clone(),
            why: c.mesh_why.clone(),
            path: std::path::PathBuf::from(&c.trigger_path),
            start: c.trigger_start.map(|v| v as u32).unwrap_or(0),
            end: c.trigger_end.map(|v| v as u32).unwrap_or(u32::MAX),
            whole: c.trigger_start.is_none() || c.trigger_end.is_none(),
            status: MeshRangeStatus::Stable,
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
        ScoreBreakdown { shared_id: 0.0, co_edit: 0.0, trigram: 0.0, composite: 1.0 },
        participants,
        String::new(),
        meta,
    )
}

/// Zero-sized struct: wraps `detect_partner_drift` behind the `Detector` trait.
pub struct PartnerDriftDetector;

impl crate::advice::detector::Detector for PartnerDriftDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_partner_drift(input).iter().map(candidate_to_suggestion).collect())
    }
}

/// Zero-sized struct: wraps `detect_read_intersects_mesh` behind the `Detector` trait.
pub struct ReadIntersectsMeshDetector;

impl crate::advice::detector::Detector for ReadIntersectsMeshDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_read_intersects_mesh(input).iter().map(candidate_to_suggestion).collect())
    }
}

/// Zero-sized struct: wraps `detect_staging_cross_cut` behind the `Detector` trait.
pub struct StagingCrossCutDetector;

impl crate::advice::detector::Detector for StagingCrossCutDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_staging_cross_cut(input).iter().map(candidate_to_suggestion).collect())
    }
}

/// Zero-sized struct: wraps `detect_delta_intersects_mesh` behind the `Detector` trait.
pub struct DeltaIntersectsMeshDetector;

impl crate::advice::detector::Detector for DeltaIntersectsMeshDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_delta_intersects_mesh(input).iter().map(candidate_to_suggestion).collect())
    }
}

/// Zero-sized struct: wraps `detect_range_shrink` behind the `Detector` trait.
pub struct RangeShrinkDetector;

impl crate::advice::detector::Detector for RangeShrinkDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_range_shrink(input).iter().map(candidate_to_suggestion).collect())
    }
}

/// Zero-sized struct: wraps `detect_rename_consequence` behind the `Detector` trait.
pub struct RenameConsequenceDetector;

impl crate::advice::detector::Detector for RenameConsequenceDetector {
    fn detect(&self, input: &CandidateInput<'_>) -> anyhow::Result<Vec<crate::advice::suggestion::Suggestion>> {
        Ok(detect_rename_consequence(input).iter().map(candidate_to_suggestion).collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::session::state::{ReadRecord, TouchInterval};
    use crate::advice::workspace_tree::DiffEntry;
    use std::path::PathBuf;

    // ── Fixture helpers ──────────────────────────────────────────────────────

    fn make_mesh_range(name: &str, path: &str, start: u32, end: u32) -> MeshRange {
        MeshRange {
            name: name.to_string(),
            why: "why text".to_string(),
            path: PathBuf::from(path),
            start,
            end,
            whole: false,
            status: MeshRangeStatus::Stable,
        }
    }

    fn make_whole_mesh_range(name: &str, path: &str) -> MeshRange {
        MeshRange {
            name: name.to_string(),
            why: "why text".to_string(),
            path: PathBuf::from(path),
            start: 0,
            end: u32::MAX,
            whole: true,
            status: MeshRangeStatus::Stable,
        }
    }

    fn make_read(path: &str, start: u32, end: u32) -> ReadRecord {
        ReadRecord {
            path: path.to_string(),
            start_line: Some(start),
            end_line: Some(end),
            ts: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn empty_input<'a>(
        delta: &'a [DiffEntry],
        reads: &'a [ReadRecord],
        touches: &'a [TouchInterval],
        ranges: &'a [MeshRange],
    ) -> CandidateInput<'a> {
        CandidateInput {
            session_delta: delta,
            incr_delta: delta,
            new_reads: reads,
            touch_intervals: touches,
            mesh_ranges: ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        }
    }

    // ── detect_read_intersects_mesh ──────────────────────────────────────────

    /// A ReadRecord whose line range overlaps a MeshRange must produce
    /// Partner Candidates for the other ranges in the same mesh.
    #[test]
    fn read_intersects_mesh_emits_partner() {
        let ranges = [
            make_mesh_range("my-mesh", "src/foo.rs", 10, 20),
            make_mesh_range("my-mesh", "src/bar.rs", 1, 5),
        ];
        let reads = [make_read("src/foo.rs", 12, 15)];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &reads,
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_read_intersects_mesh(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::Partner);
        assert_eq!(result[0].mesh, "my-mesh");
        assert_eq!(result[0].mesh_why, "why text");
        assert_eq!(result[0].partner_path, "src/bar.rs");
        assert_eq!(result[0].trigger_path, "src/foo.rs");
        assert_eq!(result[0].trigger_start, Some(12));
        assert_eq!(result[0].trigger_end, Some(15));
    }

    #[test]
    fn read_single_range_mesh_emits_nothing() {
        let ranges = [make_mesh_range("my-mesh", "src/foo.rs", 10, 20)];
        let reads = [make_read("src/foo.rs", 12, 15)];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &reads,
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_read_intersects_mesh(&input);
        assert!(result.is_empty());
    }

    /// A ReadRecord on a path not in mesh_ranges emits nothing.
    #[test]
    fn read_outside_mesh_emits_nothing() {
        let ranges = [make_mesh_range("my-mesh", "src/foo.rs", 10, 20)];
        let reads = [make_read("src/bar.rs", 12, 15)];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &reads,
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_read_intersects_mesh(&input);
        assert!(result.is_empty());
    }

    // ── detect_delta_intersects_mesh ─────────────────────────────────────────

    /// A DiffEntry::Modified on a meshed path conservatively produces partners
    /// from the other ranges in the same mesh.
    #[test]
    fn delta_modify_intersects_mesh_emits_partner() {
        let ranges = [
            make_mesh_range("net-mesh", "src/net.rs", 1, 50),
            make_mesh_range("net-mesh", "src/caller.rs", 1, 10),
        ];
        let delta = [DiffEntry::Modified {
            path: "src/net.rs".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_delta_intersects_mesh(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::Partner);
        assert_eq!(result[0].partner_path, "src/caller.rs");
        assert_eq!(result[0].trigger_path, "src/net.rs");
        assert_eq!(result[0].trigger_start, None);
        assert_eq!(result[0].trigger_end, None);
    }

    /// A DiffEntry::Modified on a path not in mesh_ranges emits nothing.
    #[test]
    fn delta_outside_mesh_emits_nothing() {
        let ranges = [make_mesh_range("net-mesh", "src/net.rs", 1, 50)];
        let delta = [DiffEntry::Modified {
            path: "src/other.rs".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_delta_intersects_mesh(&input);
        assert!(result.is_empty());
    }

    // ── detect_partner_drift ─────────────────────────────────────────────────

    /// A MeshRange with Changed status must produce a Terminal Candidate.
    #[test]
    fn partner_drift_changed_status_emits_terminal() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Changed;
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[r],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_partner_drift(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::Terminal);
        assert_eq!(result[0].mesh, "drift-mesh");
    }

    /// detect_partner_drift with Changed status must emit a Candidate with
    /// trigger_path == "" and partner_marker == "[CHANGED]" (Bug 4).
    #[test]
    fn partner_drift_changed_produces_empty_trigger_and_changed_marker() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Changed;
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[r],
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_partner_drift(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].trigger_path, "", "trigger_path must be empty for partner drift");
        assert_eq!(result[0].partner_marker, "[CHANGED]", "partner_marker must be [CHANGED]");
    }

    /// detect_partner_drift with Moved status must produce partner_marker == "[MOVED]".
    #[test]
    fn partner_drift_moved_produces_moved_marker() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Moved;
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[r],
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_partner_drift(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].partner_marker, "[MOVED]");
    }

    /// detect_partner_drift with Terminal status must produce partner_marker == "[TERMINAL]".
    #[test]
    fn partner_drift_terminal_produces_terminal_marker() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Terminal;
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[r],
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_partner_drift(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].partner_marker, "[TERMINAL]");
    }

    #[test]
    fn partner_drift_on_session_touched_path_emits_nothing() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Changed;
        let delta = [DiffEntry::Modified {
            path: "src/drift.rs".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &delta,
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[r],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(detect_partner_drift(&input).is_empty());
    }

    // ── detect_rename_consequence ────────────────────────────────────────────

    /// A session_delta Renamed entry whose `from` path is meshed must produce
    /// an L2 RenameLiteral Candidate with the rm/add/commit command.
    /// The trigger and partner_path are both the new path (`to`).
    #[test]
    fn rename_of_meshed_path_emits_rename_literal() {
        let ranges = [
            make_mesh_range("ren-mesh", "src/old.rs", 1, 10),
            make_mesh_range("ren-mesh", "src/caller.rs", 1, 5),
        ];
        let delta = [DiffEntry::Renamed {
            from: "src/old.rs".to_string(),
            to: "src/new.rs".to_string(),
            score: 95,
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &delta,
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_rename_consequence(&input);
        // One L2 candidate per meshed range that was renamed (src/old.rs).
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::RenameLiteral);
        assert_eq!(result[0].density, Density::L2);
        // Trigger and partner are the new (navigable) path.
        assert_eq!(result[0].trigger_path, "src/new.rs");
        assert_eq!(result[0].partner_path, "src/new.rs");
        // old/new paths record the rename.
        assert_eq!(result[0].old_path, Some("src/old.rs".to_string()));
        assert_eq!(result[0].new_path, Some("src/new.rs".to_string()));
        // Command carries the rm/add/commit triple.
        assert!(
            result[0].command.contains("git mesh rm  ren-mesh src/old.rs"),
            "command must include rm: {:?}",
            result[0].command
        );
        assert!(
            result[0].command.contains("git mesh add ren-mesh src/new.rs"),
            "command must include add: {:?}",
            result[0].command
        );
        assert!(
            result[0].command.contains("git mesh commit ren-mesh"),
            "command must include commit: {:?}",
            result[0].command
        );
    }

    /// Full Bug 5 scenario: Renamed { from: src/foo.ts, to: src/bar.ts } in both
    /// session_delta and incr_delta, plus a partner range on src/uses.ts.
    ///
    /// detect_delta_intersects_mesh must produce:
    ///   - one Partner candidate with partner_path="src/bar.ts", partner_marker="[RENAMED]",
    ///     partner_clause="was src/foo.ts", trigger_path="src/uses.ts"
    ///     (from processing Modified { src/uses.ts } with rename resolution)
    ///
    /// detect_rename_consequence must produce:
    ///   - one L2 RenameLiteral candidate with command containing rm/add/commit
    #[test]
    fn bug5_rename_of_meshed_path_full_scenario() {
        let ranges = [
            make_whole_mesh_range("link", "src/foo.ts"),
            make_whole_mesh_range("link", "src/uses.ts"),
        ];
        let renamed = DiffEntry::Renamed {
            from: "src/foo.ts".to_string(),
            to: "src/bar.ts".to_string(),
            score: 95,
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        };
        let modified = DiffEntry::Modified {
            path: "src/uses.ts".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        };
        let delta = [renamed, modified];
        let input = CandidateInput {
            session_delta: &delta,
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };

        // (a) delta detector: one Partner candidate with [RENAMED] annotation
        let delta_cands = detect_delta_intersects_mesh(&input);
        // Only one candidate expected: from Modified { src/uses.ts } → partner src/bar.ts
        // (Renamed { src/foo.ts } is skipped since from is meshed)
        assert_eq!(
            delta_cands.len(),
            1,
            "expected exactly one delta partner candidate; got: {delta_cands:#?}"
        );
        assert_eq!(delta_cands[0].partner_path, "src/bar.ts");
        assert_eq!(delta_cands[0].partner_marker, "[RENAMED]");
        assert_eq!(delta_cands[0].partner_clause, "was src/foo.ts");
        assert_eq!(delta_cands[0].trigger_path, "src/uses.ts");

        // (b) rename consequence detector: one L2 candidate with rm/add/commit
        let rename_cands = detect_rename_consequence(&input);
        assert_eq!(
            rename_cands.len(),
            1,
            "expected exactly one rename consequence candidate; got: {rename_cands:#?}"
        );
        assert_eq!(rename_cands[0].reason_kind, ReasonKind::RenameLiteral);
        assert_eq!(rename_cands[0].density, Density::L2);
        assert!(rename_cands[0].command.contains("git mesh rm  link src/foo.ts"));
        assert!(rename_cands[0].command.contains("git mesh add link src/bar.ts"));
        assert!(rename_cands[0].command.contains("git mesh commit link"));
    }

    // ── detect_range_shrink ──────────────────────────────────────────────────

    /// When a meshed path's blob shrinks, a RangeCollapse Candidate should be
    /// emitted — unskip when DiffEntry carries blob line counts (sub-card C).
    #[test]
    #[ignore = "deferred: detect_range_shrink requires blob line-count data (sub-card C)"]
    fn range_shrink_emits_range_collapse_when_blob_lines_decrease() {
        // DiffEntry::Modified on a path whose mesh range end > new blob line count
        let ranges = [make_mesh_range("shrink-mesh", "src/big.rs", 1, 200)];
        let delta = [DiffEntry::Modified {
            path: "src/big.rs".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        // Phase C will attach old_lines/new_lines to DiffEntry; for now the
        // detector must detect "range end exceeds new blob size" to emit RangeCollapse.
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_range_shrink(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::RangeCollapse);
    }

    // ── detect_staging_cross_cut ─────────────────────────────────────────────

    /// A staged add that overlaps an existing mesh range must produce a
    /// StagingCrossCut Candidate.
    #[test]
    fn staging_add_overlapping_existing_mesh_emits_cross_cut() {
        let ranges = [make_mesh_range("stage-mesh", "src/api.rs", 10, 50)];
        let adds = [StagedAddr {
            path: PathBuf::from("src/api.rs"),
            start: 20,
            end: 40,
            whole: false,
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &adds,
                removes: &[],
            },
        };
        let result = detect_staging_cross_cut(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::StagingCrossCut);
    }

    /// A staged remove that empties a mesh range must produce an EmptyMesh
    /// Candidate.
    #[test]
    fn staging_remove_emptying_mesh_emits_empty_mesh() {
        let ranges = [make_mesh_range("empty-mesh", "src/api.rs", 10, 50)];
        // Remove covers the entire mesh range
        let removes = [StagedAddr {
            path: PathBuf::from("src/api.rs"),
            start: 10,
            end: 50,
            whole: false,
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &removes,
            },
        };
        let result = detect_staging_cross_cut(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::EmptyMesh);
    }

    // ── detect_session_co_touch (removed in card main-13 slice 2) ────────────
    //
    // The pairwise co-touch channel was replaced by
    // `advice::suggest::run_suggest_pipeline`, which produces n-ary,
    // line-bounded recommendations. Coverage moved to
    // `tests/advice_suggest_*.rs` and `tests/advice_suggest_in_render.rs`.
    // Ensure empty_input helper is used (avoids dead_code warning in tests)
    #[test]
    fn empty_input_compiles() {
        let _i = empty_input(&[], &[], &[], &[]);
    }

    // ── whole-file pin rendering (Bug 1) ─────────────────────────────────────

    /// When the partner mesh range is whole-file (whole=true), the produced
    /// Candidate must have partner_start=None and partner_end=None so that
    /// format_addr renders it as a bare path with no #L… suffix.
    #[test]
    fn whole_file_partner_produces_none_range() {
        let trigger = make_mesh_range("checkout-flow", "web/checkout.tsx", 1, 100);
        let partner = make_whole_mesh_range("checkout-flow", "api/charge.ts");
        let delta = [DiffEntry::Modified {
            path: "web/checkout.tsx".to_string(),
            old_oid: None,
            new_oid: None,
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &[trigger, partner],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_delta_intersects_mesh(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].partner_path, "api/charge.ts");
        assert_eq!(result[0].partner_start, None, "whole-file partner must have None start");
        assert_eq!(result[0].partner_end, None, "whole-file partner must have None end");
    }

    // ── blob OID propagation (Bug 2) ─────────────────────────────────────────

    /// Two successive content edits (differing old/new OIDs) must produce two
    /// distinct fingerprints so the second edit is not suppressed.
    #[test]
    fn successive_content_edits_produce_distinct_fingerprints() {
        use crate::advice::fingerprint::fingerprint;

        let ranges = [
            make_mesh_range("checkout-flow", "web/checkout.tsx", 1, 100),
            make_mesh_range("checkout-flow", "api/charge.ts", 1, 80),
        ];

        let delta_first = [DiffEntry::Modified {
            path: "web/checkout.tsx".to_string(),
            old_oid: Some("aaa0000000000000000000000000000000000001".to_string()),
            new_oid: Some("bbb0000000000000000000000000000000000002".to_string()),
            hunks: None,
        
        }];
        let input_first = CandidateInput {
            session_delta: &[],
            incr_delta: &delta_first,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let cands_first = detect_delta_intersects_mesh(&input_first);
        assert_eq!(cands_first.len(), 1);

        let delta_second = [DiffEntry::Modified {
            path: "web/checkout.tsx".to_string(),
            old_oid: Some("bbb0000000000000000000000000000000000002".to_string()),
            new_oid: Some("ccc0000000000000000000000000000000000003".to_string()),
            hunks: None,
        
        }];
        let input_second = CandidateInput {
            session_delta: &[],
            incr_delta: &delta_second,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let cands_second = detect_delta_intersects_mesh(&input_second);
        assert_eq!(cands_second.len(), 1);

        assert_ne!(
            fingerprint(&cands_first[0]),
            fingerprint(&cands_second[0]),
            "distinct blob OIDs must yield distinct fingerprints"
        );
    }

    /// A no-op render (same delta repeated, same OIDs) produces the same
    /// fingerprint as the first render, so suppression correctly silences it.
    #[test]
    fn noop_render_produces_same_fingerprint() {
        use crate::advice::fingerprint::fingerprint;

        let ranges = [
            make_mesh_range("checkout-flow", "web/checkout.tsx", 1, 100),
            make_mesh_range("checkout-flow", "api/charge.ts", 1, 80),
        ];
        let delta = [DiffEntry::Modified {
            path: "web/checkout.tsx".to_string(),
            old_oid: Some("aaa0000000000000000000000000000000000001".to_string()),
            new_oid: Some("bbb0000000000000000000000000000000000002".to_string()),
            hunks: None,
        
        }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState { adds: &[], removes: &[] },
        };
        let cands_a = detect_delta_intersects_mesh(&input);
        let cands_b = detect_delta_intersects_mesh(&input);
        assert_eq!(cands_a.len(), 1);
        assert_eq!(cands_b.len(), 1);
        assert_eq!(
            fingerprint(&cands_a[0]),
            fingerprint(&cands_b[0]),
            "identical delta must yield identical fingerprint (suppression must fire)"
        );
    }
}
