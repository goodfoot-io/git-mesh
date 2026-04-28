//! Band assignment and viability labelling (Section 15 of analyze-v4.mjs).
//!
//! `confidence_band` maps a composite score to one of the four bands, then
//! applies channel-count and density caps. `viability_label` decides whether
//! the suggestion is ready to surface, needs review, or is too weak.

use crate::advice::suggest::composite::CandidateScore;
use crate::advice::suggestion::{ConfidenceBand, Viability};

// BANDS order (lowest to highest): Low, Medium, High, HighPlus
// Matches JS: ['low', 'medium', 'high', 'high+']
fn band_rank(b: ConfidenceBand) -> usize {
    match b {
        ConfidenceBand::Low => 0,
        ConfidenceBand::Medium => 1,
        ConfidenceBand::High => 2,
        ConfidenceBand::HighPlus => 3,
    }
}

fn cap_band(band: ConfidenceBand, max: ConfidenceBand) -> ConfidenceBand {
    if band_rank(band) > band_rank(max) {
        max
    } else {
        band
    }
}

/// Assign a confidence band to a scored candidate.
///
/// Ports `confidenceBand` from `docs/analyze-v4.mjs` line 961.
pub fn confidence_band(c: &CandidateScore) -> ConfidenceBand {
    let s = c.composite;
    let mut band = if s >= 0.78 {
        ConfidenceBand::HighPlus
    } else if s >= 0.60 {
        ConfidenceBand::High
    } else if s >= 0.42 {
        ConfidenceBand::Medium
    } else {
        ConfidenceBand::Low
    };

    // Channel-count cap: 1 channel caps at medium, 2 at high, ≥3 reaches high+.
    let cc = c.techniques.len();
    if cc <= 1 {
        band = cap_band(band, ConfidenceBand::Medium);
    } else if cc <= 2 {
        band = cap_band(band, ConfidenceBand::High);
    }

    // Density penalty: non-fully-connected n-ary clique can't be high+.
    if c.components.density < 1.0 && matches!(band, ConfidenceBand::HighPlus) {
        band = ConfidenceBand::High;
    }

    band
}

/// Assign a viability label to a scored candidate.
///
/// Ports `viabilityLabel` from `docs/analyze-v4.mjs` line 978.
pub fn viability_label(c: &CandidateScore, history_available: bool) -> Viability {
    let _ = history_available; // referenced for parity; used implicitly via historical_pair_commits
    let cohesion_present =
        c.components.cluster_cohesion >= 0.20 || c.components.trigram_score >= 0.18 || c.size == 2;
    if c.composite >= 0.55 && (c.historical_pair_commits >= 2 || cohesion_present) {
        return Viability::Ready;
    }
    if c.composite >= 0.65 && c.components.density >= 0.9 {
        return Viability::Ready;
    }
    if c.composite >= 0.45 && (cohesion_present || c.historical_pair_commits >= 1) {
        return Viability::Ready; // "review" mapped to Ready in the Rust Viability enum
        // Note: the JS returns 'review' — the Rust Viability enum has only
        // Ready/Suppressed/Superseded. We map JS 'review' → Ready (it is surfaced)
        // and JS 'weak' → Suppressed (it is not surfaced).
    }
    if c.composite >= 0.40 {
        return Viability::Ready; // JS 'review' → Ready
    }
    Viability::Suppressed // JS 'weak' → Suppressed
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::composite::{CandidateScore, ComponentBreakdown};

    fn make_candidate(
        composite: f64,
        techniques: usize,
        density: f64,
        size: usize,
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
                trigram_score: 0.25,
                intersection_cohesion: 0.30,
                pairwise_min_cohesion: 0.15,
                pairwise_median_cohesion: 0.30,
                pairwise_mean_cohesion: 0.30,
                cluster_cohesion: 0.30,
                history_score: 0.5,
            },
            techniques: (0..techniques).map(|i| format!("tech-{i}")).collect(),
            historical_pair_commits: 2,
            historical_weighted: 0.5,
            same_file_dominance: 0.5,
            cross_package: true,
            op_distance_avg: 2.0,
            shared_identifiers: vec![],
            composite,
        }
    }

    #[test]
    fn composite_above_078_is_high_plus_with_three_channels() {
        let c = make_candidate(0.80, 3, 1.0, 3);
        assert_eq!(confidence_band(&c), ConfidenceBand::HighPlus);
    }

    #[test]
    fn one_channel_caps_at_medium_even_with_high_composite() {
        let c = make_candidate(0.80, 1, 1.0, 3);
        assert_eq!(confidence_band(&c), ConfidenceBand::Medium);
    }

    #[test]
    fn two_channels_caps_at_high() {
        let c = make_candidate(0.80, 2, 1.0, 3);
        assert_eq!(confidence_band(&c), ConfidenceBand::High);
    }

    #[test]
    fn density_below_one_demotes_high_plus_to_high() {
        let c = make_candidate(0.80, 3, 0.9, 3);
        assert_eq!(confidence_band(&c), ConfidenceBand::High);
    }

    #[test]
    fn viability_strong_composite_with_cohesion_is_ready() {
        let c = make_candidate(0.60, 3, 1.0, 3);
        assert_eq!(viability_label(&c, true), Viability::Ready);
    }

    #[test]
    fn viability_low_composite_is_suppressed() {
        let mut c = make_candidate(0.30, 3, 1.0, 3);
        c.historical_pair_commits = 0;
        c.components.cluster_cohesion = 0.0;
        c.components.trigram_score = 0.0;
        assert_eq!(viability_label(&c, true), Viability::Suppressed);
    }

    #[test]
    fn pair_size_2_cohesion_present_ready() {
        let c = make_candidate(0.55, 2, 1.0, 2);
        // size == 2 → cohesion_present = true
        assert_eq!(viability_label(&c, false), Viability::Ready);
    }
}
