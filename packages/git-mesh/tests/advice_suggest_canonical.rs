//! Unit-test stubs for the canonical-id stage.
//!
//! The canonical stage uses IoU-based connected-components to assign a stable
//! canonical identifier to each participant cluster. Stability means that
//! running the algorithm twice on the same input yields the same ids, and
//! that adding a non-overlapping file does not change ids of the existing
//! cluster.
//!
//! Tests are `#[ignore]`d until Step 3 implements the canonical stage.

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
// IoU connected-components yields stable canonical ids per run
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — canonical not yet implemented"]
fn same_input_produces_same_canonical_ids() {
    // When implemented: run the canonical-id algorithm twice on identical
    // input graphs and assert all ids are equal.
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
#[ignore = "phase 3 — canonical not yet implemented"]
fn disjoint_files_get_distinct_canonical_ids() {
    // When implemented: two files with no IoU edge between them must belong
    // to different canonical components.
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
#[ignore = "phase 3 — canonical not yet implemented"]
fn connected_files_get_same_canonical_id() {
    // When implemented: two files with IoU >= range_overlap_iou must share
    // one canonical component id.
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
#[ignore = "phase 3 — canonical not yet implemented"]
fn adding_unrelated_file_does_not_change_existing_ids() {
    // When implemented: inserting a file with no edges to an existing cluster
    // must leave the existing cluster's canonical id unchanged.
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
#[ignore = "phase 3 — canonical not yet implemented"]
fn transitive_edges_expand_component() {
    // When implemented: if A-B and B-C both have IoU edges, all three belong
    // to the same canonical component even if A-C does not.
    let s = Suggestion::new(
        ConfidenceBand::High,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
