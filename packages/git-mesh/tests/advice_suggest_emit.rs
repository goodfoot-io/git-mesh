//! Unit-test stubs for the emit stage.
//!
//! The emit stage applies two final filters before a `Suggestion` reaches
//! stdout:
//!   1. Subsumption suppression: a smaller clique is Superseded if all its
//!      members appear in a larger clique that is Ready.
//!   2. Pair-escape hatch: a size-2 clique that fails the clique cohesion
//!      floor but passes `pair_cohesion_floor + pair_escape_bonus` may still
//!      be emitted as a Low-band suggestion.
//!
//! Tests are `#[ignore]`d until Step 3 implements the emit stage.

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
// Subsumption suppression
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn pair_subsumed_by_larger_clique_is_superseded() {
    // When implemented: a size-2 suggestion {A, B} must be Superseded if a
    // Ready size-3 suggestion {A, B, C} also exists in the output set.
    let pair = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Superseded,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(pair.viability, Viability::Superseded);
}

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn clique_not_subsumed_remains_ready() {
    // When implemented: a size-3 suggestion whose members do not all appear
    // in any larger Ready suggestion must remain Viability::Ready.
    let clique = Suggestion::new(
        ConfidenceBand::High,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(clique.viability, Viability::Ready);
}

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn suppressed_larger_clique_does_not_subsume_pair() {
    // When implemented: a size-3 suggestion that is Suppressed must NOT cause
    // subsumption of a constituent size-2 suggestion (only Ready cliques subsume).
    let pair = Suggestion::new(
        ConfidenceBand::Medium,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(pair.viability, Viability::Ready);
}

// ---------------------------------------------------------------------------
// Pair-escape hatch
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn pair_above_escape_threshold_is_emitted_as_low_band() {
    // When implemented: a size-2 clique that fails `clique_cohesion_floor`
    // but passes `pair_cohesion_floor + pair_escape_bonus` must be emitted
    // with ConfidenceBand::Low and Viability::Ready.
    let escape = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(escape.band, ConfidenceBand::Low);
    assert_eq!(escape.viability, Viability::Ready);
}

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn pair_below_escape_threshold_is_suppressed() {
    // When implemented: a size-2 clique that fails both `clique_cohesion_floor`
    // and `pair_cohesion_floor + pair_escape_bonus` must be Suppressed.
    let suppressed = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(suppressed.viability, Viability::Suppressed);
}

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn clique_larger_than_2_does_not_use_pair_escape() {
    // When implemented: a size-3+ clique that fails `clique_cohesion_floor`
    // must NOT receive the pair-escape bonus — it must be Suppressed outright.
    let big_suppressed = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Suppressed,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(big_suppressed.viability, Viability::Suppressed);
}

// ---------------------------------------------------------------------------
// Serde contract: Suggestion serializes with "v": 1 as first field
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — emit not yet implemented"]
fn suggestion_serde_version_witness_is_first_field() {
    // Verify the versioned JSON contract: the first key in the serialized
    // object must be "v" with value 1. This constraint is load-bearing for
    // consumers that schema-check before deserializing.
    //
    // This test does NOT need Step 3 to pass, but it lives here because it
    // validates the output contract enforced by the emit stage.
    let s = Suggestion::new(
        ConfidenceBand::Medium,
        Viability::Ready,
        ScoreBreakdown {
            shared_id: 0.1,
            co_edit: 0.2,
            trigram: 0.3,
            composite: 0.6,
        },
        vec![],
        "test label".to_string(),
    );
    let json = serde_json::to_string(&s).expect("serialize");
    // The raw JSON must start with `{"v":1,` (after the opening brace).
    assert!(
        json.starts_with(r#"{"v":1,"#),
        "expected serialized Suggestion to start with {{\"v\":1,, got: {json}"
    );
}
