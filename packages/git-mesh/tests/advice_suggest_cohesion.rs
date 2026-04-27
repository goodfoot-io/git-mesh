//! Unit-test stubs for the cohesion-scoring stage.
//!
//! Cohesion measures how tightly the participants in a candidate clique are
//! coupled. It is computed at four granularities (session co-touch, range
//! overlap, shared-identifier IDF, history co-edit) and each has a floor
//! below which the clique is suppressed (or demoted to pair-escape for size 2).
//!
//! Tests are `#[ignore]`d until Step 3 implements the cohesion stage.

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
// Four granularities each pass/fail at v4 floors
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — cohesion not yet implemented"]
fn session_co_touch_above_floor_passes() {
    // When implemented: a clique whose session co-touch ratio meets the
    // pair_cohesion_floor (0.30) must not be suppressed on that dimension.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn session_co_touch_below_floor_suppresses_clique() {
    // When implemented: a clique whose session co-touch ratio falls below
    // pair_cohesion_floor must be assigned Viability::Suppressed.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn range_overlap_iou_above_floor_passes() {
    // When implemented: a clique with mean pairwise IoU >= range_overlap_iou
    // (0.30) must not be suppressed on the range-overlap dimension.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn range_overlap_iou_below_floor_suppresses_clique() {
    // When implemented: a clique with mean pairwise IoU < range_overlap_iou
    // must be Suppressed.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn shared_id_idf_above_floor_passes() {
    // When implemented: cliques with enough shared identifiers (>= 1 saturated
    // token) must not be suppressed on the shared-id dimension.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn shared_id_idf_below_floor_suppresses_clique() {
    // When implemented: a clique with no shared identifiers and a composite
    // score below min_score must be Suppressed.
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
#[ignore = "phase 3 — cohesion not yet implemented"]
fn history_co_edit_above_floor_passes() {
    // When implemented: a clique with a history co-edit score >= edge_score_floor
    // (0.40) on all edges must not be suppressed on the history dimension.
    let s = Suggestion::new(
        ConfidenceBand::HighPlus,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}

#[test]
#[ignore = "phase 3 — cohesion not yet implemented"]
fn history_co_edit_below_floor_suppresses_clique() {
    // When implemented: a clique with a history co-edit edge below the floor
    // must be Suppressed (unless pair-escape applies).
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
