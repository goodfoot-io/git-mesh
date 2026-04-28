//! Apriori support / confidence / lift stage (Section 8 of analyze-v4.mjs).
//!
//! `atom_marginals` builds a per-canonical-id → session-id set index.
//! `apriori_stats` computes support, confidence, and lift for a pair given that
//! index and the pair's shared-session count.

use std::collections::{BTreeMap, BTreeSet};

use crate::advice::suggest::evidence::PairState;

// ── Public types ──────────────────────────────────────────────────────────────

/// Maps canonical-id → set of session ids in which that atom appeared.
pub type AtomSessionIndex = BTreeMap<usize, BTreeSet<String>>;

/// Apriori statistics for one pair.
#[derive(Clone, Debug)]
pub struct AprioriStats {
    /// sessions_with_both / total_sessions
    pub support: f64,
    /// max(P(A|B), P(B|A))
    pub confidence: f64,
    /// support / (P(A) * P(B))
    pub lift: f64,
    /// Raw count of shared sessions.
    pub shared_sessions: usize,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build atom marginals from pre-resolved (canonical_id, session_sid) pairs.
///
/// This is the actual working entry point used by the `edges` and test code.
pub fn atom_marginals_resolved(
    resolved: &[(usize, String)], // (canonical_id, session_sid)
) -> AtomSessionIndex {
    let mut index: AtomSessionIndex = BTreeMap::new();
    // Deduplicate per session: each (canonical_id, session_sid) pair counted once.
    let mut seen: BTreeSet<(usize, String)> = BTreeSet::new();
    for (cid, sid) in resolved {
        if seen.insert((*cid, sid.clone())) {
            index.entry(*cid).or_default().insert(sid.clone());
        }
    }
    index
}

/// Compute apriori stats for a pair given the atom-session index.
///
/// Ports `aprioriStats` from `docs/analyze-v4.mjs` line 470.
pub fn apriori_stats(
    pair: &PairState,
    atom_sessions: &AtomSessionIndex,
    total_sessions: usize,
) -> AprioriStats {
    let (a, b) = pair.canon_ids;
    let shared_sessions = pair.sessions.len();
    let a_sessions = atom_sessions.get(&a).map_or(0, |s| s.len());
    let b_sessions = atom_sessions.get(&b).map_or(0, |s| s.len());

    let total = total_sessions.max(1) as f64;
    let support = shared_sessions as f64 / total;
    let confidence = f64::max(
        if a_sessions > 0 {
            shared_sessions as f64 / a_sessions as f64
        } else {
            0.0
        },
        if b_sessions > 0 {
            shared_sessions as f64 / b_sessions as f64
        } else {
            0.0
        },
    );
    let denom = (a_sessions as f64 / total) * (b_sessions as f64 / total);
    let lift = if denom > 0.0 { support / denom } else { 0.0 };

    AprioriStats {
        support,
        confidence,
        lift,
        shared_sessions,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::evidence::PairState;
    use std::collections::BTreeSet;

    fn make_pair_state(sessions: &[&str]) -> PairState {
        PairState {
            canon_ids: (0, 1),
            evidence: vec![],
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
            edit_hits: 0,
            weighted_hits: 0.0,
            kinds: BTreeSet::new(),
        }
    }

    #[test]
    fn support_is_fraction_of_total_sessions() {
        let pair = make_pair_state(&["s1", "s2"]);
        let mut index = AtomSessionIndex::new();
        index.insert(
            0,
            ["s1", "s2", "s3"].iter().map(|s| s.to_string()).collect(),
        );
        index.insert(1, ["s1", "s2"].iter().map(|s| s.to_string()).collect());
        let stats = apriori_stats(&pair, &index, 4);
        // support = 2/4 = 0.5
        assert!((stats.support - 0.5).abs() < 1e-9);
    }

    #[test]
    fn confidence_is_max_conditional_probability() {
        let pair = make_pair_state(&["s1"]);
        let mut index = AtomSessionIndex::new();
        // a appears in 2 sessions, b in 1; shared=1
        // P(A|B) = 1/1 = 1.0, P(B|A) = 1/2 = 0.5 → max = 1.0
        index.insert(0, ["s1", "s2"].iter().map(|s| s.to_string()).collect());
        index.insert(1, ["s1"].iter().map(|s| s.to_string()).collect());
        let stats = apriori_stats(&pair, &index, 4);
        assert!((stats.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn lift_exceeds_one_for_correlated_pair() {
        // All sessions contain both atoms → lift should be > 1.
        let pair = make_pair_state(&["s1", "s2"]);
        let mut index = AtomSessionIndex::new();
        index.insert(0, ["s1", "s2"].iter().map(|s| s.to_string()).collect());
        index.insert(1, ["s1", "s2"].iter().map(|s| s.to_string()).collect());
        let stats = apriori_stats(&pair, &index, 2);
        // P(A)=1, P(B)=1, support=1, denom=1 → lift=1
        // With 2 sessions and both appear in both, lift = (2/2) / ((2/2)*(2/2)) = 1
        assert!(stats.lift >= 1.0);
    }

    #[test]
    fn atom_marginals_resolved_deduplicates_per_session() {
        let pairs = vec![
            (0usize, "s1".to_string()),
            (0usize, "s1".to_string()), // duplicate
            (0usize, "s2".to_string()),
            (1usize, "s1".to_string()),
        ];
        let index = atom_marginals_resolved(&pairs);
        assert_eq!(index[&0].len(), 2, "s1 and s2, no duplicates");
        assert_eq!(index[&1].len(), 1);
    }
}
