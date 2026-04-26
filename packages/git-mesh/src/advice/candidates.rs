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
    /// T7 new-group candidate.
    NewGroup,
    /// T8 staging cross-cut.
    StagingCrossCut,
    /// T9 empty-mesh risk.
    EmptyMesh,
    /// T10 pending-commit re-anchor.
    PendingCommit,
    /// T11 terminal status.
    Terminal,
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
            ReasonKind::NewGroup => "new_group",
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
            ReasonKind::NewGroup => Some("recording-a-group"),
            ReasonKind::StagingCrossCut => Some("cross-mesh-overlap"),
            ReasonKind::EmptyMesh => Some("empty-groups"),
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
    pub status: MeshRangeStatus,
}

#[derive(Debug, Clone)]
pub struct StagedAddr {
    pub path: std::path::PathBuf,
    pub start: u32,
    pub end: u32,
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
            bare_candidate(
                &partner.name,
                &partner.why,
                kind,
                (
                    &partner_path,
                    Some(partner.start as i64),
                    Some(partner.end as i64),
                ),
                (trigger_path, trigger_start, trigger_end),
            )
        })
        .collect()
}

/// Returns true if a path should be filtered from co-touch analysis.
/// Filters: generated, vendored, lockfile, binary, and common ignored patterns.
fn is_filtered_path(path: &str) -> bool {
    // Generated paths
    let components: Vec<&str> = path.split('/').collect();
    if components
        .iter()
        .any(|&c| matches!(c, "target" | "node_modules" | "dist" | "build"))
    {
        return true;
    }
    // Vendored
    if components
        .iter()
        .any(|&c| matches!(c, "vendor" | "third_party" | "third-party"))
    {
        return true;
    }
    // Lockfiles (by filename)
    if let Some(name) = components.last()
        && matches!(
            *name,
            "Cargo.lock"
                | "yarn.lock"
                | "package-lock.json"
                | "poetry.lock"
                | "Gemfile.lock"
                | "bun.lockb"
                | "composer.lock"
                | "pnpm-lock.yaml"
                | "Pipfile.lock"
        )
    {
        return true;
    }
    // Binary extensions
    if let Some(dot) = path.rfind('.') {
        let ext = &path[dot..];
        if matches!(
            ext,
            ".png"
                | ".jpg"
                | ".jpeg"
                | ".gif"
                | ".pdf"
                | ".zip"
                | ".tar"
                | ".gz"
                | ".exe"
                | ".so"
                | ".dylib"
                | ".dll"
                // Binary fonts
                | ".woff"
                | ".woff2"
                | ".ttf"
                | ".otf"
                // Binary media
                | ".mp4"
                | ".webm"
                | ".wav"
                | ".mp3"
        ) {
            return true;
        }
    }
    // Common ignored patterns: *.log files
    if path.ends_with(".log") {
        return true;
    }
    // Generated/minified patterns
    if path.ends_with(".pb.go")
        || path.ends_with(".pb.cc")
        || path.ends_with("_pb2.py")
        || path.ends_with(".min.js")
        || path.ends_with(".min.css")
        || path.ends_with(".snap")
    {
        return true;
    }
    false
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
            if ranges_overlap(read_start, read_end, range.start, range.end) {
                out.extend(partner_candidates_for_trigger(
                    input,
                    &read.path,
                    read.start_line.map(i64::from),
                    read.end_line.map(i64::from),
                    range,
                ));
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
            DiffEntry::Modified { path }
            | DiffEntry::Added { path }
            | DiffEntry::Deleted { path }
            | DiffEntry::ModeChange { path } => {
                out.extend(delta_path_partners(input, path));
            }
            DiffEntry::Renamed { from, to, .. } => {
                out.extend(delta_path_partners(input, from));
                out.extend(delta_path_partners(input, to));
            }
        }
    }
    out
}

fn delta_path_partners(input: &CandidateInput<'_>, path: &str) -> Vec<Candidate> {
    if path_is_internal(path, input.internal_path_prefixes) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for range in input.mesh_ranges {
        let range_path = range.path.to_string_lossy();
        if path == range_path.as_ref() {
            out.extend(partner_candidates_for_trigger(
                input, path, None, None, range,
            ));
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
            DiffEntry::Modified { path }
            | DiffEntry::Added { path }
            | DiffEntry::Deleted { path }
            | DiffEntry::ModeChange { path } => vec![path.as_str()],
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
            out.push(bare_candidate(
                &range.name,
                &range.why,
                ReasonKind::Terminal,
                (&path_str, Some(range.start as i64), Some(range.end as i64)),
                (&path_str, Some(range.start as i64), Some(range.end as i64)),
            ));
        }
    }
    out
}

/// Emit `RenameLiteral` for `session_delta` Renamed entries whose old path is
/// meshed.
pub fn detect_rename_consequence(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in input.session_delta {
        if let DiffEntry::Renamed { from, to, .. } = entry {
            for range in input.mesh_ranges {
                let range_path = range.path.to_string_lossy();
                if from.as_str() == range_path.as_ref() {
                    out.extend(partner_candidates_for_trigger_kind(
                        input,
                        ReasonKind::RenameLiteral,
                        from,
                        None,
                        None,
                        range,
                    ));
                }
                if to.as_str() == range_path.as_ref() {
                    out.extend(partner_candidates_for_trigger_kind(
                        input,
                        ReasonKind::RenameLiteral,
                        to,
                        None,
                        None,
                        range,
                    ));
                }
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

/// Emit `NewGroup` for co-touch pairs from `touch_intervals` that exceed
/// frequency thresholds, filtering generated/ignored/vendored/lockfile/binary.
///
/// A pair (path_a, path_b) qualifies when each path appears in
/// `touch_intervals` at least `THRESHOLD` times (a proxy for the pair
/// co-occurring in at least that many intervals). Pairs where both paths are
/// covered by a mesh range in the same mesh are skipped.
///
/// TODO sub-card C: gate on historical co-change (git log lookup out of scope
/// for this sub-card). Sub-card C will also refine interval grouping using
/// real interval IDs once `TouchInterval` carries them.
pub fn detect_session_co_touch(input: &CandidateInput<'_>) -> Vec<Candidate> {
    use std::collections::{HashMap, HashSet};

    // Count per-path occurrences across touch_intervals (filtered).
    let mut path_counts: HashMap<&str, usize> = HashMap::new();
    for t in input.touch_intervals {
        if !is_filtered_path(&t.path) && !path_is_internal(&t.path, input.internal_path_prefixes) {
            *path_counts.entry(t.path.as_str()).or_default() += 1;
        }
    }

    // Paths that appear at least THRESHOLD times are candidates for pairing.
    const THRESHOLD: usize = 2;
    let mut qualified: Vec<&str> = path_counts
        .iter()
        .filter(|(_, count)| **count >= THRESHOLD)
        .map(|(&p, _)| p)
        .collect();
    qualified.sort_unstable();

    if qualified.len() < 2 {
        return Vec::new();
    }

    // Build set of (mesh_name → paths) for existing mesh coverage check.
    // A pair is skipped if both paths appear in ranges under the same mesh name.
    let mut mesh_paths: HashMap<&str, HashSet<&str>> = HashMap::new();
    for range in input.mesh_ranges {
        let path_str = range.path.to_str().unwrap_or("");
        mesh_paths
            .entry(range.name.as_str())
            .or_default()
            .insert(path_str);
    }

    let mut out = Vec::new();

    // Enumerate unordered pairs and emit NewGroup for qualifying ones.
    for i in 0..qualified.len() {
        for j in (i + 1)..qualified.len() {
            let pa = qualified[i];
            let pb = qualified[j];

            // Skip if both are covered by the same mesh
            let already_meshed = mesh_paths
                .values()
                .any(|paths_in_mesh| paths_in_mesh.contains(pa) && paths_in_mesh.contains(pb));
            if already_meshed {
                continue;
            }

            out.push(bare_candidate(
                "",
                "",
                ReasonKind::NewGroup,
                (pa, None, None),
                (pb, None, None),
            ));
        }
    }

    out
}

/// Emit `StagingCrossCut`/`EmptyMesh` for `staging.adds`/`staging.removes`
/// vs `mesh_ranges`.
pub fn detect_staging_cross_cut(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();

    // staged adds overlapping a mesh range → StagingCrossCut
    for add in input.staging.adds {
        let add_path = add.path.to_string_lossy();
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if add_path.as_ref() != range_path.as_ref() {
                continue;
            }
            if ranges_overlap(add.start, add.end, range.start, range.end) {
                out.push(bare_candidate(
                    &range.name,
                    &range.why,
                    ReasonKind::StagingCrossCut,
                    (
                        &range_path,
                        Some(range.start as i64),
                        Some(range.end as i64),
                    ),
                    (&add_path, Some(add.start as i64), Some(add.end as i64)),
                ));
            }
        }
    }

    // staged removes fully covering a mesh range → EmptyMesh
    for remove in input.staging.removes {
        let rem_path = remove.path.to_string_lossy();
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if rem_path.as_ref() != range_path.as_ref() {
                continue;
            }
            // A remove empties the mesh if it covers the entire mesh range
            if remove.start <= range.start && remove.end >= range.end {
                out.push(bare_candidate(
                    &range.name,
                    &range.why,
                    ReasonKind::EmptyMesh,
                    (
                        &range_path,
                        Some(range.start as i64),
                        Some(range.end as i64),
                    ),
                    (
                        &rem_path,
                        Some(remove.start as i64),
                        Some(remove.end as i64),
                    ),
                ));
            }
        }
    }

    out
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

    fn make_touch(path: &str, start: u32, end: u32) -> TouchInterval {
        TouchInterval {
            path: path.to_string(),
            start_line: start,
            end_line: end,
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

    #[test]
    fn partner_drift_on_session_touched_path_emits_nothing() {
        let mut r = make_mesh_range("drift-mesh", "src/drift.rs", 5, 30);
        r.status = MeshRangeStatus::Changed;
        let delta = [DiffEntry::Modified {
            path: "src/drift.rs".to_string(),
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
    /// a RenameLiteral Candidate for the other ranges in the mesh.
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
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::RenameLiteral);
        assert_eq!(result[0].trigger_path, "src/old.rs");
        assert_eq!(result[0].partner_path, "src/caller.rs");
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

    // ── detect_session_co_touch ──────────────────────────────────────────────

    /// Two intervals in different files, both Changed multiple times beyond
    /// the co-touch threshold, must produce a NewGroup Candidate.
    #[test]
    fn co_touch_two_changed_intervals_emits_new_group() {
        // Simulate co-touch: same timestamp band for two different paths
        let touches = vec![
            make_touch("src/a.rs", 1, 20),
            make_touch("src/b.rs", 1, 20),
            make_touch("src/a.rs", 5, 15),
            make_touch("src/b.rs", 5, 15),
        ];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &touches,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_session_co_touch(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::NewGroup);
    }

    /// Three intervals (mix of change and read) co-touched above threshold
    /// produce a NewGroup Candidate covering all paths.
    #[test]
    fn co_touch_three_change_or_read_intervals_emits_new_group() {
        let touches = vec![
            make_touch("src/a.rs", 1, 10),
            make_touch("src/b.rs", 1, 10),
            make_touch("src/c.rs", 1, 10),
            make_touch("src/a.rs", 2, 8),
            make_touch("src/b.rs", 2, 8),
            make_touch("src/c.rs", 2, 8),
        ];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &touches,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_session_co_touch(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::NewGroup);
    }

    /// When the co-touch frequency is below threshold, no Candidate is emitted.
    #[test]
    fn co_touch_below_threshold_emits_nothing() {
        // Only one co-touch event — below any reasonable threshold
        let touches = vec![make_touch("src/a.rs", 1, 10), make_touch("src/b.rs", 1, 10)];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &touches,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_session_co_touch(&input);
        assert!(result.is_empty());
    }

    /// Intervals on generated, ignored, vendored, lockfile, and binary paths
    /// must be filtered out even when co-touch frequency exceeds threshold.
    #[test]
    fn co_touch_filters_generated_ignored_vendored_lockfile_binary() {
        // Each pair: a real file co-touched with a filtered file — no NewGroup
        // should be emitted for any filtered category.
        let make_many_touches = |path_a: &str, path_b: &str| -> Vec<TouchInterval> {
            (0..5)
                .flat_map(|i| {
                    vec![
                        make_touch(path_a, i * 10 + 1, i * 10 + 5),
                        make_touch(path_b, i * 10 + 1, i * 10 + 5),
                    ]
                })
                .collect()
        };

        // generated: *.pb.go style
        let generated = make_many_touches("src/real.rs", "src/proto.pb.go");
        let input_gen = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &generated,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(
            detect_session_co_touch(&input_gen).is_empty(),
            "generated not filtered"
        );

        // ignored: .gitignored files (detector must skip by pattern, e.g. *.log)
        let ign = make_many_touches("src/real.rs", "debug.log");
        let input_ign = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &ign,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(
            detect_session_co_touch(&input_ign).is_empty(),
            "ignored not filtered"
        );

        // vendored: vendor/ prefix
        let vnd = make_many_touches("src/real.rs", "vendor/dep/lib.rs");
        let input_vnd = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &vnd,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(
            detect_session_co_touch(&input_vnd).is_empty(),
            "vendored not filtered"
        );

        // lockfile: Cargo.lock / package-lock.json
        let lock = make_many_touches("src/real.rs", "Cargo.lock");
        let input_lock = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &lock,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(
            detect_session_co_touch(&input_lock).is_empty(),
            "lockfile not filtered"
        );

        // binary: *.png
        let bin = make_many_touches("src/real.rs", "assets/icon.png");
        let input_bin = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &bin,
            mesh_ranges: &[],
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        assert!(
            detect_session_co_touch(&input_bin).is_empty(),
            "binary not filtered"
        );
    }

    /// Co-touch pairs where both paths are already covered by a mesh range
    /// in the same mesh must not produce a NewGroup Candidate.
    #[test]
    fn co_touch_skips_pairs_with_existing_mesh() {
        let ranges = [
            make_mesh_range("existing-mesh", "src/a.rs", 1, 100),
            make_mesh_range("existing-mesh", "src/b.rs", 1, 100),
        ];
        let touches: Vec<TouchInterval> = (0..5)
            .flat_map(|i| {
                vec![
                    make_touch("src/a.rs", i * 10 + 1, i * 10 + 5),
                    make_touch("src/b.rs", i * 10 + 1, i * 10 + 5),
                ]
            })
            .collect();
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &touches,
            mesh_ranges: &ranges,
            internal_path_prefixes: &[],
            staging: StagingState {
                adds: &[],
                removes: &[],
            },
        };
        let result = detect_session_co_touch(&input);
        assert!(result.is_empty());
    }

    // Ensure empty_input helper is used (avoids dead_code warning in tests)
    #[test]
    fn empty_input_compiles() {
        let _i = empty_input(&[], &[], &[], &[]);
    }
}
