//! Tests for the composite scoring and cohesion gate (Sections 13–14 of
//! `docs/analyze-v4.mjs`).

use git_mesh::advice::suggest::{
    CandidateScore, ComponentBreakdown, SuggestConfig, passes_cohesion_gate,
};

fn cfg() -> SuggestConfig {
    SuggestConfig::default()
}

fn make_candidate(
    size: usize,
    pw_min: f64,
    intersection: f64,
    pw_median: f64,
    trigram: f64,
) -> CandidateScore {
    CandidateScore {
        canon_ids: (0..size).collect(),
        size,
        distinct_files: size,
        sessions: 2,
        components: ComponentBreakdown {
            mean_edge_score: 0.5,
            density: 1.0,
            diversity_factor: 1.0,
            edit_hits: 1,
            trigram_score: trigram,
            intersection_cohesion: intersection,
            pairwise_min_cohesion: pw_min,
            pairwise_median_cohesion: pw_median,
            pairwise_mean_cohesion: pw_median,
            cluster_cohesion: intersection.max(pw_median),
            history_score: 0.5,
        },
        techniques: vec!["tech".to_string()],
        historical_pair_commits: 0,
        historical_weighted: 0.0,
        same_file_dominance: 1.0 / size as f64,
        cross_package: true,
        op_distance_avg: 2.0,
        shared_identifiers: vec![],
        composite: 0.55,
    }
}

// ---------------------------------------------------------------------------
// Cohesion gate — pairs
// ---------------------------------------------------------------------------

#[test]
fn pair_always_passes_cohesion_gate() {
    // size == 2 → gate always passes (edge-score floor is the only gate for pairs).
    let c = make_candidate(2, 0.0, 0.0, 0.0, 0.0);
    assert!(passes_cohesion_gate(&c, &cfg()));
}

// ---------------------------------------------------------------------------
// Cohesion gate — cliques (size ≥ 3)
// ---------------------------------------------------------------------------

#[test]
fn clique_with_zero_pairwise_min_fails_hard_floor() {
    // pairwise_min < 0.10 → always fails regardless of other values.
    let c = make_candidate(3, 0.0, 0.50, 0.50, 0.50);
    assert!(!passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn clique_passes_via_intersection_cohesion() {
    // pw_min=0.15 (≥0.10), intersection=0.35 (≥clique_floor=0.30)
    let c = make_candidate(3, 0.15, 0.35, 0.10, 0.10);
    assert!(passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn clique_passes_via_pairwise_median() {
    // pw_min=0.15, pw_median=0.45 (≥0.30+0.10=0.40)
    let c = make_candidate(3, 0.15, 0.05, 0.45, 0.10);
    assert!(passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn clique_passes_via_pairwise_min_high() {
    // pw_min=0.55 (≥0.30+0.20=0.50)
    let c = make_candidate(3, 0.55, 0.05, 0.10, 0.10);
    assert!(passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn clique_passes_via_trigram_score() {
    // pw_min=0.15, trigram=0.25 (≥0.20)
    let c = make_candidate(3, 0.15, 0.05, 0.05, 0.25);
    assert!(passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn clique_failing_all_sub_gates_fails() {
    // pw_min=0.15 (OK), intersection=0.10, pw_median=0.20, trigram=0.10 — none pass
    let c = make_candidate(3, 0.15, 0.10, 0.20, 0.10);
    assert!(!passes_cohesion_gate(&c, &cfg()));
}

#[test]
fn gate_uses_config_clique_cohesion_floor() {
    // With clique_cohesion_floor=0.10, intersection=0.12 now passes.
    let custom_cfg = SuggestConfig {
        clique_cohesion_floor: 0.10,
        ..SuggestConfig::default()
    };
    let c = make_candidate(3, 0.15, 0.12, 0.05, 0.05);
    assert!(passes_cohesion_gate(&c, &custom_cfg));
}
