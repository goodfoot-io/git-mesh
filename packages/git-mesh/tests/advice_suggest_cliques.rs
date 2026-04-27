//! Unit-test stubs for the clique-enumeration stage.
//!
//! The clique stage runs a Bron-Kerbosch maximal-clique enumeration on the
//! edge graph produced by the participants stage, then caps results at
//! `max_clique_size` (default 8).
//!
//! Tests are `#[ignore]`d until Step 3 implements the clique stage.

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
// Bron-Kerbosch on hand-built 4–6 node graphs
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — cliques not yet implemented"]
fn complete_4_node_graph_yields_one_4_clique() {
    // When implemented: a complete graph K4 (all 6 edges present) must yield
    // exactly one maximal clique of size 4.
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
#[ignore = "phase 3 — cliques not yet implemented"]
fn two_triangles_sharing_one_edge_yields_two_cliques() {
    // When implemented: nodes {A,B,C,D} with edges AB,AC,BC,BD,CD must yield
    // two maximal 3-cliques: {A,B,C} and {B,C,D}.
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
#[ignore = "phase 3 — cliques not yet implemented"]
fn complete_6_node_graph_yields_one_6_clique() {
    // When implemented: K6 must yield exactly one maximal clique of size 6,
    // and no smaller cliques since they are all subsumed.
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
#[ignore = "phase 3 — cliques not yet implemented"]
fn path_graph_4_nodes_yields_edge_cliques_only() {
    // When implemented: path graph A-B-C-D must yield three maximal 2-cliques
    // (AB, BC, CD) — no triangles.
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
// size cap 8
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — cliques not yet implemented"]
fn clique_larger_than_max_size_is_truncated() {
    // When implemented: a K9 input graph must not produce a 9-node clique;
    // all emitted cliques must have size <= max_clique_size (8).
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
#[ignore = "phase 3 — cliques not yet implemented"]
fn clique_exactly_at_max_size_is_kept() {
    // When implemented: a K8 input graph must produce exactly one 8-clique
    // (not truncated, not discarded).
    let s = Suggestion::new(
        ConfidenceBand::HighPlus,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
