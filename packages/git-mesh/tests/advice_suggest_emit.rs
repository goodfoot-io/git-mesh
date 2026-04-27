//! Tests for the emit stage.
//!
//! Ports Section 16 of `docs/analyze-v4.mjs`: isSuperset and emit.

use std::collections::BTreeMap;
use git_mesh::advice::suggest::{emit, CanonicalIndex, CanonicalRange, CandidateScore, ComponentBreakdown, SuggestConfig};
use git_mesh::advice::suggestion::{ConfidenceBand, Viability};

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

fn make_candidate(ids: Vec<usize>, composite: f64, pw_min: f64, intersect: f64, pw_med: f64, trigram: f64) -> CandidateScore {
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
            intersection_cohesion: intersect,
            pairwise_min_cohesion: pw_min,
            pairwise_median_cohesion: pw_med,
            pairwise_mean_cohesion: pw_med,
            cluster_cohesion: intersect.max(pw_med),
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

// ---------------------------------------------------------------------------
// Subsumption suppression
// ---------------------------------------------------------------------------

#[test]
fn pair_subsumed_by_larger_clique_is_dropped() {
    let canonical = make_canonical(4);
    // Clique {0,1,2}: passes gate (pw_min≥0.10, intersection≥0.30)
    let clique = make_candidate(vec![0, 1, 2], 0.65, 0.20, 0.35, 0.20, 0.10);
    // Pair {0,1}: subsumed, composite 0.55, does not escape (0.55 < 0.65 + 0.20)
    let pair = make_candidate(vec![0, 1], 0.55, 0.0, 0.0, 0.0, 0.0);
    let result = emit(vec![clique, pair], &canonical, &cfg(), true);
    assert_eq!(result.len(), 1, "pair must be dropped by subsumption");
    assert_eq!(result[0].participants.len(), 3);
}

#[test]
fn pair_not_subsumed_remains_in_output() {
    let canonical = make_canonical(4);
    let pair = make_candidate(vec![2, 3], 0.55, 0.0, 0.0, 0.0, 0.0);
    let result = emit(vec![pair], &canonical, &cfg(), true);
    assert_eq!(result.len(), 1);
}

#[test]
fn larger_clique_suppresses_smaller_contained_clique() {
    let canonical = make_canonical(5);
    let big = make_candidate(vec![0, 1, 2, 3], 0.70, 0.20, 0.35, 0.20, 0.10);
    let small = make_candidate(vec![0, 1, 2], 0.65, 0.20, 0.35, 0.20, 0.10);
    let result = emit(vec![big, small], &canonical, &cfg(), true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].participants.len(), 4);
}

// ---------------------------------------------------------------------------
// Pair-escape hatch
// ---------------------------------------------------------------------------

#[test]
fn pair_escapes_when_composite_beats_container_by_bonus() {
    let canonical = make_canonical(4);
    let clique = make_candidate(vec![0, 1, 2], 0.50, 0.20, 0.35, 0.20, 0.10);
    // pair composite = 0.71 ≥ 0.50 + 0.20 = 0.70
    let pair = make_candidate(vec![0, 1], 0.71, 0.0, 0.0, 0.0, 0.0);
    let result = emit(vec![clique, pair], &canonical, &cfg(), true);
    assert_eq!(result.len(), 2, "pair must escape because it beats container by ≥ bonus");
}

#[test]
fn pair_below_escape_threshold_is_not_emitted() {
    let canonical = make_canonical(4);
    let clique = make_candidate(vec![0, 1, 2], 0.60, 0.20, 0.35, 0.20, 0.10);
    // pair composite = 0.65 < 0.60 + 0.20 = 0.80
    let pair = make_candidate(vec![0, 1], 0.65, 0.0, 0.0, 0.0, 0.0);
    let result = emit(vec![clique, pair], &canonical, &cfg(), true);
    assert_eq!(result.len(), 1);
}

// ---------------------------------------------------------------------------
// Cohesion gate enforcement on cliques
// ---------------------------------------------------------------------------

#[test]
fn clique_failing_cohesion_gate_excluded() {
    let canonical = make_canonical(4);
    // pw_min=0.0 → hard floor fails
    let bad_clique = make_candidate(vec![0, 1, 2], 0.70, 0.0, 0.0, 0.0, 0.0);
    let result = emit(vec![bad_clique], &canonical, &cfg(), true);
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// Serde contract
// ---------------------------------------------------------------------------

#[test]
fn suggestion_serde_version_witness_is_first_field() {
    use git_mesh::advice::suggestion::{ScoreBreakdown, Suggestion};
    let s = Suggestion::new(
        ConfidenceBand::Medium,
        Viability::Ready,
        ScoreBreakdown { shared_id: 0.1, co_edit: 0.2, trigram: 0.3, composite: 0.6 },
        vec![],
        "test label".to_string(),
    );
    let json = serde_json::to_string(&s).expect("serialize");
    assert!(
        json.starts_with(r#"{"v":1,"#),
        "expected serialized Suggestion to start with {{\"v\":1,, got: {json}"
    );
}

#[test]
fn emit_output_participants_match_canon_ids() {
    let canonical = make_canonical(3);
    let candidate = make_candidate(vec![0, 1, 2], 0.65, 0.20, 0.35, 0.20, 0.10);
    let result = emit(vec![candidate], &canonical, &cfg(), true);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].participants.len(), 3);
}
