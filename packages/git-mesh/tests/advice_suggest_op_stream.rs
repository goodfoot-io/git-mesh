//! Unit-test stubs for the op-stream stage (dump-drop and edit-coalesce).
//!
//! These tests are `#[ignore]`d until Step 3 implements the op-stream pipeline.
//! Bodies use only types from Step 1 to keep the file compile-clean.

use git_mesh::advice::suggestion::{ConfidenceBand, ScoreBreakdown, Suggestion, Viability};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Construct a minimal `ScoreBreakdown` with all zeros for compile-only stubs.
fn zero_score() -> ScoreBreakdown {
    ScoreBreakdown {
        shared_id: 0.0,
        co_edit: 0.0,
        trigram: 0.0,
        composite: 0.0,
    }
}

// ---------------------------------------------------------------------------
// dump-drop: ops that are pure navigation (cursor movement without edit intent)
// must be dropped from the op-stream before scoring.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — op-stream not yet implemented"]
fn dump_op_is_dropped_from_stream() {
    // When implemented: push a dump-class op into the stream builder and assert
    // it does not appear in the emitted op sequence.
    //
    // For now, exercise the Step 1 contract to keep the file compile-green.
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
#[ignore = "phase 3 — op-stream not yet implemented"]
fn drop_op_is_dropped_from_stream() {
    // When implemented: push a drop-class op and assert it is excluded.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

// ---------------------------------------------------------------------------
// edit-coalesce: consecutive edit ops on the same file within the merge
// tolerance must be merged into a single representative interval.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — op-stream not yet implemented"]
fn adjacent_edits_within_tolerance_are_coalesced() {
    // When implemented: push two edit ops on the same file separated by
    // fewer than `range_merge_tolerance` lines; assert one interval emerges.
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
#[ignore = "phase 3 — op-stream not yet implemented"]
fn edits_beyond_tolerance_remain_separate() {
    // When implemented: push two edit ops separated by > range_merge_tolerance;
    // assert two distinct intervals emerge.
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
#[ignore = "phase 3 — op-stream not yet implemented"]
fn edit_weight_bump_applied_to_coalesced_interval() {
    // When implemented: assert coalesced edit intervals carry the
    // `edit_weight_bump` factor used downstream in scoring.
    let s = Suggestion::new(
        ConfidenceBand::High,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
