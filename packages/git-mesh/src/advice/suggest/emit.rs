//! Emission stage (Section 16 of analyze-v4.mjs).
//!
//! N-ary cliques preferred; pairs escape only when their composite beats every
//! containing clique by at least `pair_escape_bonus`. Subsumption suppression
//! is deterministic.

use crate::advice::suggest::band::{confidence_band, viability_label};
use crate::advice::suggest::canonical::CanonicalIndex;
use crate::advice::suggest::composite::{passes_cohesion_gate, CandidateScore};
use crate::advice::suggest::SuggestConfig;
use crate::advice::candidates::MeshAnchor;
use crate::advice::suggestion::{ScoreBreakdown, Suggestion};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true if `big` strictly contains all members of `small`.
///
/// Ports `isSuperset` from `docs/analyze-v4.mjs` line 997.
fn is_superset(big: &[usize], small: &[usize]) -> bool {
    if big.len() <= small.len() {
        return false;
    }
    small.iter().all(|x| big.contains(x))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Emit scored suggestions from a list of scored candidates.
///
/// Ports `emit` from `docs/analyze-v4.mjs` line 1003. N-ary cliques are
/// preferred; pairs are included only if not subsumed or if they escape via
/// `pair_escape_bonus`.
///
/// History availability is passed to `viability_label`.
pub fn emit(
    candidates: Vec<CandidateScore>,
    canonical: &CanonicalIndex,
    cfg: &SuggestConfig,
    history_available: bool,
) -> Vec<Suggestion> {
    // Partition into cliques (size ≥ 3) and pairs (size == 2).
    // Cliques must pass the cohesion gate to be kept.
    let mut cliques: Vec<CandidateScore> = candidates
        .iter()
        .filter(|c| c.size >= 3 && passes_cohesion_gate(c, cfg))
        .cloned()
        .collect();
    // Sort descending by composite for deterministic subsumption.
    cliques.sort_by(|a, b| {
        b.composite
            .partial_cmp(&a.composite)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.canon_ids.cmp(&b.canon_ids))
    });

    let mut pairs: Vec<CandidateScore> = candidates
        .iter()
        .filter(|c| c.size == 2)
        .cloned()
        .collect();
    pairs.sort_by(|a, b| {
        b.composite
            .partial_cmp(&a.composite)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.canon_ids.cmp(&b.canon_ids))
    });

    let mut kept: Vec<CandidateScore> = Vec::new();

    // Step 1: keep cliques, dropping any that are subsumed by a larger already-kept clique.
    for c in cliques {
        let ids_str = c.canon_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        if kept.iter().any(|k| is_superset(&k.canon_ids, &c.canon_ids)) {
            crate::advice_debug!(
                "suggester-drop",
                "canonical_ids" => ids_str,
                "composite" => c.composite,
                "reason" => "viability:Superseded"
            );
            continue;
        }
        crate::advice_debug!(
            "suggester-emit",
            "canonical_ids" => ids_str,
            "composite" => c.composite,
            "viability" => "Strong"
        );
        kept.push(c);
    }

    // Step 2: pair escape hatch.
    for p in pairs {
        let ids_str = p.canon_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let containers: Vec<&CandidateScore> = kept
            .iter()
            .filter(|k| is_superset(&k.canon_ids, &p.canon_ids))
            .collect();
        if containers.is_empty() {
            crate::advice_debug!(
                "suggester-emit",
                "canonical_ids" => ids_str,
                "composite" => p.composite,
                "viability" => "Strong"
            );
            kept.push(p);
            continue;
        }
        let best = containers
            .iter()
            .map(|c| c.composite)
            .fold(f64::NEG_INFINITY, f64::max);
        if p.composite >= best + cfg.pair_escape_bonus {
            crate::advice_debug!(
                "suggester-emit",
                "canonical_ids" => ids_str,
                "composite" => p.composite,
                "viability" => "Strong"
            );
            kept.push(p);
        } else {
            crate::advice_debug!(
                "suggester-drop",
                "canonical_ids" => ids_str,
                "composite" => p.composite,
                "reason" => "viability:Suppressed"
            );
        }
    }

    // Convert kept candidates to Suggestions.
    kept.into_iter()
        .map(|c| candidate_to_suggestion(c, canonical, cfg, history_available))
        .collect()
}

fn candidate_to_suggestion(
    c: CandidateScore,
    canonical: &CanonicalIndex,
    cfg: &SuggestConfig,
    history_available: bool,
) -> Suggestion {
    use crate::advice::candidates::MeshAnchorStatus;
    use std::path::PathBuf;

    let band = confidence_band(&c);
    let viability = viability_label(&c, history_available);

    let score = ScoreBreakdown {
        shared_id: c.components.intersection_cohesion,
        co_edit: c.components.history_score,
        trigram: c.components.trigram_score,
        composite: c.composite,
    };

    let participants: Vec<MeshAnchor> = c
        .canon_ids
        .iter()
        .filter_map(|&id| canonical.ranges.get(id))
        .map(|r| MeshAnchor {
            name: String::new(),
            why: String::new(),
            path: PathBuf::from(&r.path),
            start: r.start,
            end: r.end,
            whole: false,
            status: MeshAnchorStatus::Stable,
        })
        .collect();

    // Label: join paths for transparency.
    let label = c
        .canon_ids
        .iter()
        .filter_map(|&id| canonical.ranges.get(id))
        .map(|r| format!("{}#L{}-L{}", r.path, r.start, r.end))
        .collect::<Vec<_>>()
        .join(" + ");

    let _ = cfg; // top_n / min_score filtering done by caller if needed

    Suggestion::new(band, viability, score, participants, label)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::canonical::{CanonicalIndex, CanonicalRange};
    use crate::advice::suggest::composite::{CandidateScore, ComponentBreakdown};
    use std::collections::BTreeMap;

    fn cfg() -> SuggestConfig {
        SuggestConfig::default()
    }

    fn make_canonical(n: usize) -> CanonicalIndex {
        let ranges: Vec<CanonicalRange> = (0..n)
            .map(|i| CanonicalRange {
                path: format!("file{i}.rs"),
                start: 1,
                end: 10,
            })
            .collect();
        CanonicalIndex {
            ranges,
            canonical_id_of: BTreeMap::new(),
        }
    }

    fn make_candidate(ids: Vec<usize>, composite: f64, pairwise_min: f64, intersection: f64, pairwise_median: f64, trigram: f64) -> CandidateScore {
        let size = ids.len();
        CandidateScore {
            canon_ids: ids,
            size,
            distinct_files: size,
            sessions: 2,
            components: ComponentBreakdown {
                mean_edge_score: 0.5,
                density: 1.0,
                diversity_factor: 1.0,
                edit_hits: 2,
                trigram_score: trigram,
                intersection_cohesion: intersection,
                pairwise_min_cohesion: pairwise_min,
                pairwise_median_cohesion: pairwise_median,
                pairwise_mean_cohesion: pairwise_median,
                cluster_cohesion: intersection.max(pairwise_median),
                history_score: 0.5,
            },
            techniques: vec!["tech-a".to_string(), "tech-b".to_string(), "tech-c".to_string()],
            historical_pair_commits: 2,
            historical_weighted: 0.5,
            same_file_dominance: 1.0 / size as f64,
            cross_package: true,
            op_distance_avg: 2.0,
            shared_identifiers: vec![],
            composite,
        }
    }

    #[test]
    fn pair_not_subsumed_by_any_clique_is_kept() {
        let canonical = make_canonical(4);
        let pair = make_candidate(vec![0, 1], 0.55, 0.0, 0.0, 0.0, 0.0);
        let result = emit(vec![pair], &canonical, &cfg(), true);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn pair_subsumed_by_kept_clique_is_dropped() {
        let canonical = make_canonical(4);
        // Clique {0,1,2} passes cohesion gate (size≥3, pairwise_min≥0.10, intersection≥clique_floor).
        let clique = make_candidate(vec![0, 1, 2], 0.65, 0.20, 0.35, 0.20, 0.10);
        // Pair {0,1} subsumed by the clique.
        let pair = make_candidate(vec![0, 1], 0.55, 0.0, 0.0, 0.0, 0.0);
        let result = emit(vec![clique, pair], &canonical, &cfg(), true);
        // Only the clique is kept; pair is dropped (no escape: 0.55 < 0.65 + 0.20).
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].participants.len(), 3);
    }

    #[test]
    fn pair_escapes_when_composite_beats_container_by_escape_bonus() {
        let canonical = make_canonical(4);
        // Clique {0,1,2} composite=0.50.
        let clique = make_candidate(vec![0, 1, 2], 0.50, 0.20, 0.35, 0.20, 0.10);
        // Pair {0,1} composite=0.71 ≥ 0.50 + 0.20 = 0.70 → escapes.
        let pair = make_candidate(vec![0, 1], 0.71, 0.0, 0.0, 0.0, 0.0);
        let result = emit(vec![clique, pair], &canonical, &cfg(), true);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn clique_failing_cohesion_gate_is_excluded() {
        let canonical = make_canonical(4);
        // pairwise_min=0.0 → fails gate for size≥3.
        let bad_clique = make_candidate(vec![0, 1, 2], 0.70, 0.0, 0.0, 0.0, 0.0);
        let result = emit(vec![bad_clique], &canonical, &cfg(), true);
        assert!(result.is_empty());
    }

    #[test]
    fn larger_clique_suppresses_smaller_contained_clique() {
        let canonical = make_canonical(5);
        // 4-clique {0,1,2,3} and 3-clique {0,1,2}: the 3-clique is subsumed.
        let big = make_candidate(vec![0, 1, 2, 3], 0.70, 0.20, 0.35, 0.20, 0.10);
        let small = make_candidate(vec![0, 1, 2], 0.65, 0.20, 0.35, 0.20, 0.10);
        let result = emit(vec![big, small], &canonical, &cfg(), true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].participants.len(), 4);
    }

    #[test]
    fn suggestion_has_version_one() {
        let canonical = make_canonical(4);
        let pair = make_candidate(vec![0, 1], 0.60, 0.0, 0.0, 0.0, 0.0);
        let result = emit(vec![pair], &canonical, &cfg(), true);
        assert_eq!(result[0].version, 1);
    }
}
