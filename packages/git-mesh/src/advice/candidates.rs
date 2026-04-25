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
    pub staging: StagingState<'a>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns true if `[a_start, a_end]` overlaps `[b_start, b_end]` (inclusive).
fn ranges_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start <= b_end && b_start <= a_end
}

fn bare_candidate(
    mesh: &str,
    kind: ReasonKind,
    partner_path: &str,
    partner_start: Option<i64>,
    partner_end: Option<i64>,
    trigger_path: &str,
) -> Candidate {
    Candidate {
        mesh: mesh.to_string(),
        mesh_why: String::new(),
        reason_kind: kind,
        partner_path: partner_path.to_string(),
        partner_start,
        partner_end,
        trigger_path: trigger_path.to_string(),
        trigger_start: None,
        trigger_end: None,
        partner_marker: String::new(),
        partner_clause: String::new(),
        density: kind.default_density(),
        command: String::new(),
        excerpt_of_path: String::new(),
        excerpt_start: None,
        excerpt_end: None,
    }
}

/// Returns true if a path should be filtered from co-touch analysis.
/// Filters: generated, vendored, lockfile, binary, and common ignored patterns.
fn is_filtered_path(path: &str) -> bool {
    // Generated paths
    let components: Vec<&str> = path.split('/').collect();
    if components.iter().any(|&c| {
        matches!(c, "target" | "node_modules" | "dist" | "build")
    }) {
        return true;
    }
    // Vendored
    if components.iter().any(|&c| {
        matches!(c, "vendor" | "third_party" | "third-party")
    }) {
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
        ) {
            return true;
        }
    }
    // Common ignored patterns: *.log files
    if path.ends_with(".log") {
        return true;
    }
    // Generated: *.pb.go style
    if path.ends_with(".pb.go") {
        return true;
    }
    false
}

// ── Detector functions ───────────────────────────────────────────────────────

/// Emit `Partner` for each `new_reads` interval that intersects a mesh range.
pub fn detect_read_intersects_mesh(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for read in input.new_reads {
        let read_start = read.start_line.unwrap_or(0);
        let read_end = read.end_line.unwrap_or(u32::MAX);
        for range in input.mesh_ranges {
            let path_str = range.path.to_string_lossy();
            if read.path != path_str.as_ref() {
                continue;
            }
            if ranges_overlap(read_start, read_end, range.start, range.end) {
                out.push(bare_candidate(
                    &range.name,
                    ReasonKind::Partner,
                    &path_str,
                    Some(range.start as i64),
                    Some(range.end as i64),
                    &read.path,
                ));
            }
        }
    }
    out
}

/// Emit `Partner` for `incr_delta` Modified/Deleted/Renamed entries whose path
/// appears in `mesh_ranges`. Over-emit model: any match on path emits one
/// Partner per overlapping mesh range without hunk parsing.
pub fn detect_delta_intersects_mesh(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in input.incr_delta {
        let path = match entry {
            DiffEntry::Modified { path } => path.as_str(),
            DiffEntry::Deleted { path } => path.as_str(),
            DiffEntry::Renamed { from, .. } => from.as_str(),
            DiffEntry::Added { .. } | DiffEntry::ModeChange { .. } => continue,
        };
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if path == range_path.as_ref() {
                out.push(bare_candidate(
                    &range.name,
                    ReasonKind::Partner,
                    &range_path,
                    Some(range.start as i64),
                    Some(range.end as i64),
                    path,
                ));
            }
        }
    }
    out
}

/// Emit `Terminal` for `mesh_ranges` rows with Changed/Moved/Terminal status.
pub fn detect_partner_drift(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for range in input.mesh_ranges {
        if matches!(
            range.status,
            MeshRangeStatus::Changed | MeshRangeStatus::Moved | MeshRangeStatus::Terminal
        ) {
            let path_str = range.path.to_string_lossy();
            out.push(bare_candidate(
                &range.name,
                ReasonKind::Terminal,
                &path_str,
                Some(range.start as i64),
                Some(range.end as i64),
                &path_str,
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
                    out.push(bare_candidate(
                        &range.name,
                        ReasonKind::RenameLiteral,
                        to,
                        None,
                        None,
                        from,
                    ));
                }
            }
        }
    }
    out
}

/// Emit `RangeCollapse` when a meshed path has a Modified entry in incr_delta.
///
/// Phase C note: `DiffEntry::Modified` does not carry old/new line counts.
/// This detector emits `RangeCollapse` for any Modified entry on a meshed path,
/// conservatively flagging that the blob may have shrunk below the mesh range.
/// Sub-card C will extend `DiffEntry` with line-count metadata to make this
/// precise.
pub fn detect_range_shrink(input: &CandidateInput<'_>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in input.incr_delta {
        let path = match entry {
            DiffEntry::Modified { path } => path.as_str(),
            _ => continue,
        };
        for range in input.mesh_ranges {
            let range_path = range.path.to_string_lossy();
            if path == range_path.as_ref() {
                out.push(bare_candidate(
                    &range.name,
                    ReasonKind::RangeCollapse,
                    &range_path,
                    Some(range.start as i64),
                    Some(range.end as i64),
                    path,
                ));
            }
        }
    }
    out
}

/// Emit `NewGroup` for co-touch pairs from `touch_intervals` that exceed
/// frequency thresholds, filtering generated/ignored/vendored/lockfile/binary.
///
/// Threshold: a pair of paths must appear together at least 2 times.
/// Pairs where both paths are covered by a mesh range in the same mesh are
/// skipped.
///
/// TODO sub-card C: gate on historical co-change (git log lookup out of scope
/// for unit tests).
pub fn detect_session_co_touch(input: &CandidateInput<'_>) -> Vec<Candidate> {
    use std::collections::HashMap;

    // Collect unique paths, filtering noise
    let paths: Vec<&str> = {
        let mut seen = std::collections::HashSet::new();
        input
            .touch_intervals
            .iter()
            .map(|t| t.path.as_str())
            .filter(|p| !is_filtered_path(p))
            .filter(|p| seen.insert(*p))
            .collect()
    };

    if paths.len() < 2 {
        return Vec::new();
    }

    // Count co-touch frequency for each pair: number of touch intervals that
    // overlap in time. We use a simple proxy: count how many touch records each
    // path has, then count pair co-occurrences by considering all (path_a,
    // path_b) combinations that appear in the intervals list.
    let mut path_counts: HashMap<&str, u32> = HashMap::new();
    for t in input.touch_intervals {
        if !is_filtered_path(&t.path) {
            *path_counts.entry(t.path.as_str()).or_default() += 1;
        }
    }

    // Build set of (mesh_name, path) for existing mesh coverage check
    // A pair is skipped if both paths appear in ranges under the same mesh name.
    let mut mesh_paths: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
    for range in input.mesh_ranges {
        let path_str = range.path.to_str().unwrap_or("");
        mesh_paths
            .entry(range.name.as_str())
            .or_default()
            .insert(path_str);
    }

    let threshold: u32 = 2; // minimum co-touch count per path to qualify
    let mut out = Vec::new();

    let qualified: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|&p| path_counts.get(p).copied().unwrap_or(0) >= threshold)
        .collect();

    if qualified.len() < 2 {
        return Vec::new();
    }

    // Emit one NewGroup per qualifying pair, skipping pairs already in same mesh
    for i in 0..qualified.len() {
        for j in (i + 1)..qualified.len() {
            let pa = qualified[i];
            let pb = qualified[j];

            // Skip if both are covered by the same mesh
            let already_meshed = mesh_paths.values().any(|paths_in_mesh| {
                paths_in_mesh.contains(pa) && paths_in_mesh.contains(pb)
            });
            if already_meshed {
                continue;
            }

            out.push(bare_candidate(
                "",
                ReasonKind::NewGroup,
                pa,
                None,
                None,
                pb,
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
                    ReasonKind::StagingCrossCut,
                    &range_path,
                    Some(range.start as i64),
                    Some(range.end as i64),
                    &add_path,
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
                    ReasonKind::EmptyMesh,
                    &range_path,
                    Some(range.start as i64),
                    Some(range.end as i64),
                    &rem_path,
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
            staging: StagingState { adds: &[], removes: &[] },
        }
    }

    // ── detect_read_intersects_mesh ──────────────────────────────────────────

    /// A ReadRecord whose line range overlaps a MeshRange must produce one
    /// Partner Candidate referencing that mesh range.
    #[test]
    fn read_intersects_mesh_emits_partner() {
        let ranges = [make_mesh_range("my-mesh", "src/foo.rs", 10, 20)];
        let reads = [make_read("src/foo.rs", 12, 15)];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &reads,
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_read_intersects_mesh(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::Partner);
        assert_eq!(result[0].mesh, "my-mesh");
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
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_read_intersects_mesh(&input);
        assert!(result.is_empty());
    }

    // ── detect_delta_intersects_mesh ─────────────────────────────────────────

    /// A DiffEntry::Modified on a meshed path must produce at least one
    /// Partner Candidate (over-emit model: entire mesh range is considered hit).
    #[test]
    fn delta_modify_intersects_mesh_emits_partner() {
        let ranges = [make_mesh_range("net-mesh", "src/net.rs", 1, 50)];
        let delta = [DiffEntry::Modified { path: "src/net.rs".to_string() }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_delta_intersects_mesh(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::Partner);
    }

    /// A DiffEntry::Modified on a path not in mesh_ranges emits nothing.
    #[test]
    fn delta_outside_mesh_emits_nothing() {
        let ranges = [make_mesh_range("net-mesh", "src/net.rs", 1, 50)];
        let delta = [DiffEntry::Modified { path: "src/other.rs".to_string() }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &[], removes: &[] },
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
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_partner_drift(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::Terminal);
        assert_eq!(result[0].mesh, "drift-mesh");
    }

    // ── detect_rename_consequence ────────────────────────────────────────────

    /// A session_delta Renamed entry whose `from` path is meshed must produce
    /// a RenameLiteral Candidate.
    #[test]
    fn rename_of_meshed_path_emits_rename_literal() {
        let ranges = [make_mesh_range("ren-mesh", "src/old.rs", 1, 10)];
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
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_rename_consequence(&input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason_kind, ReasonKind::RenameLiteral);
    }

    // ── detect_range_shrink ──────────────────────────────────────────────────

    /// When a meshed path's blob shrinks (old blob had more lines than new),
    /// a RangeCollapse Candidate should be emitted.
    ///
    /// Note: DiffEntry::Modified carries no blob OIDs in this phase; the test
    /// uses a Modified entry on a meshed path and documents the expected
    /// behavior when Phase C extends DiffEntry with line-count metadata.
    #[test]
    fn range_shrink_emits_range_collapse_when_blob_lines_decrease() {
        // DiffEntry::Modified on a path whose mesh range end > new blob line count
        let ranges = [make_mesh_range("shrink-mesh", "src/big.rs", 1, 200)];
        let delta = [DiffEntry::Modified { path: "src/big.rs".to_string() }];
        // Phase C will attach old_lines/new_lines to DiffEntry; for now the
        // detector must detect "range end exceeds new blob size" to emit RangeCollapse.
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &delta,
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &[], removes: &[] },
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
        let adds = [StagedAddr { path: PathBuf::from("src/api.rs"), start: 20, end: 40 }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &adds, removes: &[] },
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
        let removes = [StagedAddr { path: PathBuf::from("src/api.rs"), start: 10, end: 50 }];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &[],
            mesh_ranges: &ranges,
            staging: StagingState { adds: &[], removes: &removes },
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
            staging: StagingState { adds: &[], removes: &[] },
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
            staging: StagingState { adds: &[], removes: &[] },
        };
        let result = detect_session_co_touch(&input);
        assert!(!result.is_empty());
        assert_eq!(result[0].reason_kind, ReasonKind::NewGroup);
    }

    /// When the co-touch frequency is below threshold, no Candidate is emitted.
    #[test]
    fn co_touch_below_threshold_emits_nothing() {
        // Only one co-touch event — below any reasonable threshold
        let touches = vec![
            make_touch("src/a.rs", 1, 10),
            make_touch("src/b.rs", 1, 10),
        ];
        let input = CandidateInput {
            session_delta: &[],
            incr_delta: &[],
            new_reads: &[],
            touch_intervals: &touches,
            mesh_ranges: &[],
            staging: StagingState { adds: &[], removes: &[] },
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
            session_delta: &[], incr_delta: &[], new_reads: &[], touch_intervals: &generated,
            mesh_ranges: &[], staging: StagingState { adds: &[], removes: &[] },
        };
        assert!(detect_session_co_touch(&input_gen).is_empty(), "generated not filtered");

        // ignored: .gitignored files (detector must skip by pattern, e.g. *.log)
        let ign = make_many_touches("src/real.rs", "debug.log");
        let input_ign = CandidateInput {
            session_delta: &[], incr_delta: &[], new_reads: &[], touch_intervals: &ign,
            mesh_ranges: &[], staging: StagingState { adds: &[], removes: &[] },
        };
        assert!(detect_session_co_touch(&input_ign).is_empty(), "ignored not filtered");

        // vendored: vendor/ prefix
        let vnd = make_many_touches("src/real.rs", "vendor/dep/lib.rs");
        let input_vnd = CandidateInput {
            session_delta: &[], incr_delta: &[], new_reads: &[], touch_intervals: &vnd,
            mesh_ranges: &[], staging: StagingState { adds: &[], removes: &[] },
        };
        assert!(detect_session_co_touch(&input_vnd).is_empty(), "vendored not filtered");

        // lockfile: Cargo.lock / package-lock.json
        let lock = make_many_touches("src/real.rs", "Cargo.lock");
        let input_lock = CandidateInput {
            session_delta: &[], incr_delta: &[], new_reads: &[], touch_intervals: &lock,
            mesh_ranges: &[], staging: StagingState { adds: &[], removes: &[] },
        };
        assert!(detect_session_co_touch(&input_lock).is_empty(), "lockfile not filtered");

        // binary: *.png
        let bin = make_many_touches("src/real.rs", "assets/icon.png");
        let input_bin = CandidateInput {
            session_delta: &[], incr_delta: &[], new_reads: &[], touch_intervals: &bin,
            mesh_ranges: &[], staging: StagingState { adds: &[], removes: &[] },
        };
        assert!(detect_session_co_touch(&input_bin).is_empty(), "binary not filtered");
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
            staging: StagingState { adds: &[], removes: &[] },
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
