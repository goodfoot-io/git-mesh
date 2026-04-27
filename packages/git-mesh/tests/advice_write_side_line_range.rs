//! Slice 1 contract tests for honoring line ranges on the write side of
//! `git mesh advice` partner advisories.
//!
//! These tests pin the public-API boundary: `DiffEntry` carries an optional
//! per-entry `hunks: Vec<LineRange>` field, and `detect_delta_intersects_mesh`
//! consults it so partner candidates only fire when an edit actually touches
//! the meshed line range.
//!
//! Each test is `#[ignore]`d in this slice because the real producers populate
//! `hunks: None` (the no-false-negative fallback) — wiring up actual hunk
//! extraction from `gix` / `git diff` is the next slice. The tests compile
//! today and are discovered by `cargo nextest`, so the contract is observable
//! and the bodies can be unskipped without further plumbing once production
//! diff producers start emitting `Some(hunks)`.

use std::path::PathBuf;

use git_mesh::advice::{
    CandidateInput, DiffEntry, LineRange, MeshRange, MeshRangeStatus, ReasonKind, StagingState,
    detect_delta_intersects_mesh,
};

fn line_bounded_mesh(name: &str, path: &str, start: u32, end: u32) -> MeshRange {
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

fn whole_file_mesh(name: &str, path: &str) -> MeshRange {
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

fn input_with_delta<'a>(
    delta: &'a [DiffEntry],
    ranges: &'a [MeshRange],
) -> CandidateInput<'a> {
    CandidateInput {
        session_delta: &[],
        incr_delta: delta,
        new_reads: &[],
        touch_intervals: &[],
        mesh_ranges: ranges,
        internal_path_prefixes: &[],
        staging: StagingState { adds: &[], removes: &[] },
    }
}

/// A Modified entry whose hunks overlap the meshed line range emits the
/// partner candidate.
#[test]
#[ignore = "slice-1: contract pinned, body stubbed"]
fn write_inside_mesh_range_fires_partner() {
    let ranges = [
        line_bounded_mesh("net-mesh", "src/net.rs", 100, 150),
        line_bounded_mesh("net-mesh", "src/caller.rs", 1, 10),
    ];
    let delta = [DiffEntry::Modified {
        path: "src/net.rs".to_string(),
        old_oid: None,
        new_oid: None,
        hunks: Some(vec![LineRange { start: 110, end: 120 }]),
    }];
    let input = input_with_delta(&delta, &ranges);
    let result = detect_delta_intersects_mesh(&input);
    assert!(
        result.iter().any(|c| c.reason_kind == ReasonKind::Partner
            && c.partner_path == "src/caller.rs"),
        "expected Partner candidate for src/caller.rs, got {result:?}"
    );
}

/// A Modified entry whose hunks lie entirely outside the meshed line range
/// emits no partner candidate.
#[test]
#[ignore = "slice-1: contract pinned, body stubbed"]
fn write_outside_mesh_range_suppresses_partner() {
    let ranges = [
        line_bounded_mesh("net-mesh", "src/net.rs", 100, 150),
        line_bounded_mesh("net-mesh", "src/caller.rs", 1, 10),
    ];
    let delta = [DiffEntry::Modified {
        path: "src/net.rs".to_string(),
        old_oid: None,
        new_oid: None,
        hunks: Some(vec![LineRange { start: 200, end: 210 }]),
    }];
    let input = input_with_delta(&delta, &ranges);
    let result = detect_delta_intersects_mesh(&input);
    assert!(
        result.is_empty(),
        "expected no candidates for out-of-range edit, got {result:?}"
    );
}

/// A whole-file mesh range fires the partner candidate regardless of the
/// edit's hunks.
#[test]
#[ignore = "slice-1: contract pinned, body stubbed"]
fn whole_file_mesh_always_fires() {
    let ranges = [
        whole_file_mesh("link", "src/foo.ts"),
        whole_file_mesh("link", "src/uses.ts"),
    ];
    let delta = [DiffEntry::Modified {
        path: "src/foo.ts".to_string(),
        old_oid: None,
        new_oid: None,
        hunks: Some(vec![LineRange { start: 9999, end: 10000 }]),
    }];
    let input = input_with_delta(&delta, &ranges);
    let result = detect_delta_intersects_mesh(&input);
    assert!(
        result
            .iter()
            .any(|c| c.reason_kind == ReasonKind::Partner && c.partner_path == "src/uses.ts"),
        "expected whole-file mesh to fire regardless of hunks, got {result:?}"
    );
}

/// `hunks: None` (unknown hunks) preserves the no-false-negative invariant:
/// the partner candidate still fires.
#[test]
#[ignore = "slice-1: contract pinned, body stubbed"]
fn unknown_hunks_fall_back_to_fire() {
    let ranges = [
        line_bounded_mesh("net-mesh", "src/net.rs", 100, 150),
        line_bounded_mesh("net-mesh", "src/caller.rs", 1, 10),
    ];
    let delta = [DiffEntry::Modified {
        path: "src/net.rs".to_string(),
        old_oid: None,
        new_oid: None,
        hunks: None,
    }];
    let input = input_with_delta(&delta, &ranges);
    let result = detect_delta_intersects_mesh(&input);
    assert!(
        result
            .iter()
            .any(|c| c.reason_kind == ReasonKind::Partner && c.partner_path == "src/caller.rs"),
        "unknown hunks must fall back to firing (no false negatives), got {result:?}"
    );
}
