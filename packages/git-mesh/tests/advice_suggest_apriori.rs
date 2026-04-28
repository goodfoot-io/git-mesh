//! Integration tests for the apriori support/confidence/lift stage.

use git_mesh::advice::suggest::{apriori_stats, atom_marginals_resolved};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// atom_marginals_resolved
// ---------------------------------------------------------------------------

#[test]
fn marginals_counts_each_id_once_per_session() {
    // atom 0 in s1 twice → should count as 1.
    let pairs = vec![
        (0usize, "s1".to_string()),
        (0usize, "s1".to_string()),
        (0usize, "s2".to_string()),
    ];
    let index = atom_marginals_resolved(&pairs);
    assert_eq!(index[&0].len(), 2, "s1 and s2, no duplicate");
}

#[test]
fn marginals_absent_id_not_in_index() {
    let pairs = vec![(0usize, "s1".to_string())];
    let index = atom_marginals_resolved(&pairs);
    assert!(!index.contains_key(&99));
}

// ---------------------------------------------------------------------------
// apriori_stats
// ---------------------------------------------------------------------------

#[test]
fn support_confidence_lift_zero_for_empty_pair() {
    use git_mesh::advice::suggest::evidence::PairState;
    let pair = PairState {
        canon_ids: (0, 1),
        evidence: vec![],
        sessions: BTreeSet::new(), // 0 shared sessions
        edit_hits: 0,
        weighted_hits: 0.0,
        kinds: BTreeSet::new(),
    };
    let index = atom_marginals_resolved(&[]);
    let stats = apriori_stats(&pair, &index, 10);
    assert_eq!(stats.shared_sessions, 0);
    assert_eq!(stats.support, 0.0);
    assert_eq!(stats.confidence, 0.0);
    assert_eq!(stats.lift, 0.0);
}

#[test]
fn lift_is_greater_than_one_when_strongly_correlated() {
    use git_mesh::advice::suggest::evidence::PairState;
    // 2 sessions, both contain atoms 0 and 1 → all marginals equal 2.
    // P(A)=P(B)=1, support=1, denom=1 → lift=1.
    // But with 10 total sessions: P(A)=0.2, P(B)=0.2, support=0.2, denom=0.04 → lift=5.
    let mut sessions = BTreeSet::new();
    sessions.insert("s1".to_string());
    sessions.insert("s2".to_string());
    let pair = PairState {
        canon_ids: (0, 1),
        evidence: vec![],
        sessions,
        edit_hits: 0,
        weighted_hits: 0.0,
        kinds: BTreeSet::new(),
    };
    let index = atom_marginals_resolved(&[
        (0, "s1".to_string()),
        (0, "s2".to_string()),
        (1, "s1".to_string()),
        (1, "s2".to_string()),
    ]);
    let stats = apriori_stats(&pair, &index, 10);
    assert!(stats.lift > 1.0, "lift={} should be > 1", stats.lift);
}

#[test]
fn confidence_picks_maximum_direction() {
    use git_mesh::advice::suggest::evidence::PairState;
    // A in 5 sessions, B in 2, shared = 2 → P(B|A)=2/5=0.4, P(A|B)=2/2=1.0 → max=1.0
    let mut sessions = BTreeSet::new();
    sessions.insert("s1".to_string());
    sessions.insert("s2".to_string());
    let pair = PairState {
        canon_ids: (0, 1),
        evidence: vec![],
        sessions,
        edit_hits: 0,
        weighted_hits: 0.0,
        kinds: BTreeSet::new(),
    };
    let index = atom_marginals_resolved(&[
        (0, "s1".to_string()),
        (0, "s2".to_string()),
        (0, "s3".to_string()),
        (0, "s4".to_string()),
        (0, "s5".to_string()),
        (1, "s1".to_string()),
        (1, "s2".to_string()),
    ]);
    let stats = apriori_stats(&pair, &index, 10);
    assert!(
        (stats.confidence - 1.0).abs() < 1e-9,
        "max(P(A|B),P(B|A)) should be 1.0"
    );
}
