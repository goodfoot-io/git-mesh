//! Unit-test stubs for the participants stage.
//!
//! Participants is the per-file range-merge step that turns raw intervals
//! from the op-stream into one canonical participant record per file,
//! then computes pairwise IoU (intersection-over-union) for edge building.
//!
//! Tests are `#[ignore]`d until Step 3 implements the participants stage.

use git_mesh::advice::suggestion::{ConfidenceBand, ScoreBreakdown, Suggestion, Viability};

fn zero_score() -> ScoreBreakdown {
    ScoreBreakdown {
        shared_id: 0.0,
        co_edit: 0.0,
        trigram: 0.0,
        composite: 0.0,
    }
}

// ---------------------------------------------------------------------------
// per-file range merge tolerance
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn overlapping_intervals_merged_into_one_participant() {
    // When implemented: two intervals [1,20] and [15,35] on the same file
    // must be merged into a single [1,35] participant record.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn intervals_within_tolerance_merged_into_one_participant() {
    // When implemented: two intervals [1,10] and [15,25] on the same file,
    // with a gap of 4 lines and `range_merge_tolerance` = 5, must be merged.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn intervals_beyond_tolerance_remain_separate_participants() {
    // When implemented: two intervals with a gap > range_merge_tolerance stay
    // as separate participant records for the same file.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

// ---------------------------------------------------------------------------
// IoU edge cases
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn iou_of_identical_intervals_is_one() {
    // When implemented: two sessions both touching [10, 30] on a file yield
    // IoU = 1.0 for that file pair.
    let s = Suggestion::new(
        ConfidenceBand::High,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn iou_of_non_overlapping_intervals_is_zero() {
    // When implemented: sessions touching [1,10] and [20,30] on a file yield
    // IoU = 0.0.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn iou_partial_overlap_is_computed_correctly() {
    // When implemented: sessions touching [1,20] and [10,30] on a file yield
    // IoU = 10/30 ≈ 0.333.
    let s = Suggestion::new(
        ConfidenceBand::Medium,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — participants not yet implemented"]
fn iou_below_threshold_does_not_create_edge() {
    // When implemented: an IoU below `range_overlap_iou` (0.30) must not
    // produce an edge between the two file-participant nodes.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
