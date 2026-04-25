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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::workspace_tree::DiffEntry;
    use crate::advice::session::state::{ReadRecord, TouchInterval};
    use std::path::PathBuf;

    // ── Fixture helpers ──────────────────────────────────────────────────────

    #[allow(dead_code)]
    fn make_mesh_range(name: &str, path: &str, start: u32, end: u32) -> MeshRange {
        MeshRange {
            name: name.to_string(),
            path: PathBuf::from(path),
            start,
            end,
            status: MeshRangeStatus::Stable,
        }
    }

    #[allow(dead_code)]
    fn make_read(path: &str, start: u32, end: u32) -> ReadRecord {
        ReadRecord {
            path: path.to_string(),
            start_line: Some(start),
            end_line: Some(end),
            ts: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[allow(dead_code)]
    fn make_touch(path: &str, start: u32, end: u32) -> TouchInterval {
        TouchInterval {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            ts: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[allow(dead_code)]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
}
