//! Unit-test stubs for the locator stage.
//!
//! The locator assigns each anchored edit to the nearest prior ranged read
//! within the locator window, applying a directory-crossing penalty.
//!
//! Tests are `#[ignore]`d until Step 3 implements the locator.

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
// anchored edit attaches to nearest prior ranged read
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — locator not yet implemented"]
fn edit_attaches_to_nearest_prior_ranged_read() {
    // When implemented: given a sequence [read A, read B, edit C] where B is
    // closer to C in op distance, the locator must attach C to B.
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
#[ignore = "phase 3 — locator not yet implemented"]
fn edit_with_no_prior_read_in_window_is_unanchored() {
    // When implemented: an edit with no prior read within `locator_window`
    // ops must be left unanchored (no attachment).
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
// directory-penalty respected
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — locator not yet implemented"]
fn directory_penalty_reduces_score_for_cross_dir_attachment() {
    // When implemented: an edit in `src/a/foo.rs` attached to a read in
    // `src/b/bar.rs` (different directory) must carry the `locator_dir_penalty`
    // reduction in the edge weight used for scoring.
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
#[ignore = "phase 3 — locator not yet implemented"]
fn same_directory_attachment_has_no_penalty() {
    // When implemented: an edit in `src/a/foo.rs` attached to a read in
    // `src/a/bar.rs` (same directory) must carry no directory penalty.
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
#[ignore = "phase 3 — locator not yet implemented"]
fn prior_context_k_limits_candidates_considered() {
    // When implemented: only the `locator_prior_context_k` most-recent reads
    // before an edit are eligible for attachment.
    let s = Suggestion::new(
        ConfidenceBand::Low,
        Viability::Ready,
        zero_score(),
        vec![],
        String::new(),
    );
    assert_eq!(s.version, 1);
}
