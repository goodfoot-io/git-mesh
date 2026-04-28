//! Edge scoring stage (Section 10 of analyze-v4.mjs).
//!
//! Assembles a composite score for each pair in the `PairEvidenceMap` using
//! the apriori, history, and evidence channels. Per-edge cohesion is a `None`
//! seam — the cohesion module (Step 3c substep 9) fills it in.

use crate::advice::suggest::apriori::{apriori_stats, AtomSessionIndex};
use crate::advice::suggest::canonical::CanonicalIndex;
use crate::advice::suggest::evidence::{PairEvidenceMap, SessionParticipants, Technique};
use crate::advice::suggest::history::{pair_history_score, HistoryIndex};
use crate::advice::suggest::SuggestConfig;

// ── Public types ──────────────────────────────────────────────────────────────

/// Component score breakdown (each in [0,1]).
#[derive(Clone, Debug)]
pub struct ComponentScores {
    pub s_recurrence: f64,
    pub s_confidence: f64,
    pub s_lift: f64,
    pub s_distance: f64,
    pub s_edit: f64,
    pub s_kind: f64,
    pub s_history: f64,
}

/// One scored edge between two canonical ranges.
#[derive(Clone, Debug)]
pub struct Edge {
    /// Canonical id of the "lower" anchor (a < b).
    pub canonical_a: usize,
    /// Canonical id of the "higher" anchor.
    pub canonical_b: usize,
    /// Composite score (pre-content-cohesion).
    pub score: f64,
    /// Component breakdown.
    pub components: ComponentScores,
    /// Per-edge content cohesion — `None` here; filled in by the cohesion stage.
    pub per_edge_cohesion: Option<f64>,

    // Diagnostics (mirrors JS output fields).
    pub shared_sessions: usize,
    pub mean_op_distance: f64,
    pub lift: f64,
    pub confidence: f64,
    pub support: f64,
    pub edit_hits: u32,
    pub weighted_hits: f64,
    pub kinds: Vec<String>,
    pub history_pair_commits: u32,
    pub history_weighted: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Score all pairs and return edges above `edge_score_floor`.
///
/// Ports `scoreEdges` from `docs/analyze-v4.mjs` line 561.
pub fn score_edges(
    pairs: &PairEvidenceMap,
    sessions: &[SessionParticipants],
    canonical: &CanonicalIndex,
    atom_sessions: &AtomSessionIndex,
    history: &HistoryIndex,
    cfg: &SuggestConfig,
) -> Vec<Edge> {
    let total_sessions = sessions.len().max(1);
    let mut edges = Vec::new();

    for pair in pairs.values() {
        let (a, b) = pair.canon_ids;
        let a_range = match canonical.ranges.get(a) {
            Some(r) => r,
            None => continue,
        };
        let b_range = match canonical.ranges.get(b) {
            Some(r) => r,
            None => continue,
        };
        // Skip same-file pairs.
        if a_range.path == b_range.path {
            continue;
        }

        let apriori = apriori_stats(pair, atom_sessions, total_sessions);

        // Mean op distance from operation-window and locator-edit-context evidence.
        let op_distances: Vec<f64> = pair
            .evidence
            .iter()
            .filter(|e| {
                e.technique == Technique::OperationWindow
                    || e.technique == Technique::LocatorEditContext
            })
            .map(|e| e.op_distance as f64)
            .filter(|d| d.is_finite())
            .collect();
        let mean_distance = if op_distances.is_empty() {
            cfg.window_ops as f64
        } else {
            op_distances.iter().sum::<f64>() / op_distances.len() as f64
        };

        let (hist_count, hist_weighted) =
            pair_history_score(history, &a_range.path, &b_range.path);

        // Component scores in [0,1].
        let window_ops = cfg.window_ops as f64;
        let s_recurrence = (apriori.shared_sessions as f64 / 2.0).min(1.0);
        let s_confidence = apriori.confidence.min(1.0);
        let s_lift = (apriori.lift.max(1.0).log2()).min(3.0) / 3.0;
        let s_distance = 1.0 - (mean_distance.min(window_ops) / (window_ops + 1.0));
        let s_edit = (pair.edit_hits as f64 / 3.0).min(1.0);
        let s_kind = (pair.kinds.len() as f64 / 4.0).min(1.0);
        let s_history = if history.available {
            (hist_weighted / cfg.history_saturation as f64).min(1.0)
        } else {
            0.5
        };

        // Weighted composite (weights sum to 0.88; 0.12 reserved for cohesion).
        let score = 0.18 * s_recurrence
            + 0.14 * s_confidence
            + 0.10 * s_lift
            + 0.14 * s_distance
            + 0.12 * s_edit
            + 0.10 * s_kind
            + 0.10 * s_history;

        if score < cfg.edge_score_floor {
            continue;
        }

        let mut kinds_sorted: Vec<String> = pair
            .kinds
            .iter()
            .map(|k| match k {
                Technique::OperationWindow => "operation-window".to_string(),
                Technique::LocatorEditContext => "locator-edit-context".to_string(),
                Technique::SessionRecurrence => "session-recurrence".to_string(),
            })
            .collect();
        kinds_sorted.sort();

        edges.push(Edge {
            canonical_a: a,
            canonical_b: b,
            score,
            components: ComponentScores {
                s_recurrence,
                s_confidence,
                s_lift,
                s_distance,
                s_edit,
                s_kind,
                s_history,
            },
            per_edge_cohesion: None,
            shared_sessions: apriori.shared_sessions,
            mean_op_distance: mean_distance,
            lift: apriori.lift,
            confidence: apriori.confidence,
            support: apriori.support,
            edit_hits: pair.edit_hits,
            weighted_hits: pair.weighted_hits,
            kinds: kinds_sorted,
            history_pair_commits: hist_count,
            history_weighted: hist_weighted,
        });
    }

    edges
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::apriori::atom_marginals_resolved;
    use crate::advice::suggest::canonical::build_canonical_ranges;
    use crate::advice::suggest::evidence::{build_pair_evidence, SessionParticipants};
    use crate::advice::suggest::history::HistoryIndex;
    use crate::advice::suggest::op_stream::{Op, OpKind};
    use crate::advice::suggest::participants::{Participant, ParticipantKind};
    use crate::advice::suggest::SuggestConfig;

    fn cfg() -> SuggestConfig {
        // Lower floor so test edges pass through.
        SuggestConfig {
            edge_score_floor: 0.0,
            ..SuggestConfig::default()
        }
    }

    fn make_read_op(path: &str, start: u32, end: u32, idx: usize) -> Op {
        Op {
            path: path.to_string(),
            start_line: Some(start),
            end_line: Some(end),
            ts_ms: idx as i64,
            op_index: idx,
            kind: OpKind::Read,
            ranged: true,
            count: 1,
            inferred_start: None,
            inferred_end: None,
            locator_distance: None,
            locator_forward: None,
        }
    }

    fn make_part(path: &str, start: u32, end: u32, sid: &str, op_index: usize) -> Participant {
        Participant {
            path: path.to_string(),
            start,
            end,
            op_index,
            kind: ParticipantKind::Read,
            m_start: start,
            m_end: end,
            anchored: false,
            locator_distance: None,
            locator_forward: None,
            session_sid: sid.to_string(),
        }
    }

    fn make_session(sid: &str, parts: Vec<Participant>) -> SessionParticipants {
        let ops: Vec<Op> = parts
            .iter()
            .map(|p| make_read_op(&p.path, p.m_start, p.m_end, p.op_index))
            .collect();
        SessionParticipants {
            sid: sid.to_string(),
            ops,
            parts,
        }
    }

    #[test]
    fn score_edges_produces_edge_for_cross_file_pair() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

        // Build atom session index from participants.
        let resolved: Vec<(usize, String)> = all_parts
            .iter()
            .filter_map(|p| {
                let key = crate::advice::suggest::canonical::part_key(p);
                canonical.canonical_id_of.get(&key).map(|&id| (id, p.session_sid.clone()))
            })
            .collect();
        let atom_sessions = atom_marginals_resolved(&resolved);

        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        let history = HistoryIndex::default();

        let edges = score_edges(&pairs, &sessions, &canonical, &atom_sessions, &history, &cfg());
        assert_eq!(edges.len(), 1, "should produce one edge");
        let edge = &edges[0];
        assert!(edge.score >= 0.0);
        assert!(edge.per_edge_cohesion.is_none(), "cohesion seam must be None");
    }

    #[test]
    fn same_file_pair_is_excluded() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("a.rs", 30, 50, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

        let resolved: Vec<(usize, String)> = all_parts
            .iter()
            .filter_map(|p| {
                let key = crate::advice::suggest::canonical::part_key(p);
                canonical.canonical_id_of.get(&key).map(|&id| (id, p.session_sid.clone()))
            })
            .collect();
        let atom_sessions = atom_marginals_resolved(&resolved);

        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        let history = HistoryIndex::default();

        let edges = score_edges(&pairs, &sessions, &canonical, &atom_sessions, &history, &cfg());
        assert!(edges.is_empty(), "same-file pairs must be excluded");
    }

    #[test]
    fn edge_score_floor_filters_low_scores() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 4); // distance = 4, max window = 5
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

        let resolved: Vec<(usize, String)> = all_parts
            .iter()
            .filter_map(|p| {
                let key = crate::advice::suggest::canonical::part_key(p);
                canonical.canonical_id_of.get(&key).map(|&id| (id, p.session_sid.clone()))
            })
            .collect();
        let atom_sessions = atom_marginals_resolved(&resolved);
        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        let history = HistoryIndex::default();

        // With floor = 0.99 practically nothing passes.
        let high_floor_cfg = SuggestConfig {
            edge_score_floor: 0.99,
            ..SuggestConfig::default()
        };
        let edges = score_edges(
            &pairs,
            &sessions,
            &canonical,
            &atom_sessions,
            &history,
            &high_floor_cfg,
        );
        assert!(edges.is_empty(), "score below floor must be excluded");
    }
}
