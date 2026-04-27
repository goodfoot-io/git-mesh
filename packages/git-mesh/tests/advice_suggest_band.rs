//! Tests for the band + viability stage (Section 15 of `docs/analyze-v4.mjs`).

use git_mesh::advice::suggest::{confidence_band, viability_label, CandidateScore, ComponentBreakdown};
use git_mesh::advice::suggestion::{ConfidenceBand, Viability};

fn make_candidate(
    composite: f64,
    techniques: usize,
    density: f64,
    size: usize,
    cluster_cohesion: f64,
    trigram: f64,
    hist_commits: u32,
) -> CandidateScore {
    CandidateScore {
        canon_ids: (0..size).collect(),
        size,
        distinct_files: size,
        sessions: 2,
        components: ComponentBreakdown {
            mean_edge_score: 0.5,
            density,
            diversity_factor: 1.0,
            edit_hits: 2,
            trigram_score: trigram,
            intersection_cohesion: cluster_cohesion,
            pairwise_min_cohesion: 0.15,
            pairwise_median_cohesion: cluster_cohesion,
            pairwise_mean_cohesion: cluster_cohesion,
            cluster_cohesion,
            history_score: 0.5,
        },
        techniques: (0..techniques).map(|i| format!("tech-{i}")).collect(),
        historical_pair_commits: hist_commits,
        historical_weighted: 0.5,
        same_file_dominance: 1.0 / size as f64,
        cross_package: true,
        op_distance_avg: 2.0,
        shared_identifiers: vec![],
        composite,
    }
}

// ---------------------------------------------------------------------------
// confidence_band
// ---------------------------------------------------------------------------

#[test]
fn composite_below_042_is_low() {
    let c = make_candidate(0.30, 3, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::Low);
}

#[test]
fn composite_042_to_060_is_medium() {
    let c = make_candidate(0.50, 3, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::Medium);
}

#[test]
fn composite_060_to_078_is_high() {
    let c = make_candidate(0.70, 3, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::High);
}

#[test]
fn composite_above_078_with_three_channels_and_full_density_is_high_plus() {
    let c = make_candidate(0.80, 3, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::HighPlus);
}

#[test]
fn one_channel_caps_at_medium() {
    let c = make_candidate(0.80, 1, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::Medium);
}

#[test]
fn two_channels_caps_at_high() {
    let c = make_candidate(0.80, 2, 1.0, 3, 0.3, 0.2, 2);
    assert_eq!(confidence_band(&c), ConfidenceBand::High);
}

#[test]
fn non_full_density_prevents_high_plus() {
    let c = make_candidate(0.80, 3, 0.8, 4, 0.3, 0.2, 2);
    // density < 1.0 with high+ → demoted to high
    assert_eq!(confidence_band(&c), ConfidenceBand::High);
}

// ---------------------------------------------------------------------------
// viability_label
// ---------------------------------------------------------------------------

#[test]
fn high_composite_with_cohesion_is_ready() {
    let c = make_candidate(0.60, 3, 1.0, 3, 0.25, 0.20, 2);
    assert_eq!(viability_label(&c, true), Viability::Ready);
}

#[test]
fn composite_below_040_no_history_no_cohesion_is_suppressed() {
    let c = make_candidate(0.30, 3, 1.0, 3, 0.0, 0.0, 0);
    assert_eq!(viability_label(&c, true), Viability::Suppressed);
}

#[test]
fn pair_size_2_with_sufficient_composite_is_ready() {
    // size == 2 → cohesion_present = true (pair escape)
    let c = make_candidate(0.55, 2, 1.0, 2, 0.0, 0.0, 0);
    assert_eq!(viability_label(&c, false), Viability::Ready);
}

#[test]
fn composite_040_to_045_without_cohesion_or_history_is_review() {
    // composite ≥ 0.40 → 'review' (maps to Ready)
    let c = make_candidate(0.42, 3, 1.0, 3, 0.0, 0.0, 0);
    assert_eq!(viability_label(&c, true), Viability::Ready);
}
