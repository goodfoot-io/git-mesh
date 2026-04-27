//! n-ary mesh suggestion detector.
//!
//! `SuggestDetector` implements the `Detector` trait and will eventually
//! run the full v4 pipeline (trigram scoring, history channel,
//! clique enumeration). In this initial phase it returns an empty vec so
//! the surrounding infrastructure can be validated independently.

pub mod apriori;
pub mod band;
pub mod canonical;
pub mod cliques;
pub mod cohesion;
pub mod composite;
pub mod edges;
pub mod emit;
pub mod evidence;
pub mod history;
pub mod locator;
pub mod op_stream;
pub mod participants;

pub use apriori::{atom_marginals_resolved, apriori_stats, AtomSessionIndex, AprioriStats};
pub use band::{confidence_band, viability_label};
pub use canonical::{build_canonical_ranges, range_iou, CanonicalIndex, CanonicalRange};
pub use cliques::{build_edge_adjacency, bron_kerbosch, connected_components, edges_within, Adjacency};
pub use cohesion::{
    build_idf, cache_range, jaccard, per_edge_cohesion, range_tokens_of, read_range,
    tokens_of, trigram_cohesion, trigrams_of, CanonicalId, Idf, RangeTokens, SourceCache,
};
pub use composite::{passes_cohesion_gate, score_candidate, CandidateScore, ComponentBreakdown};
pub use edges::{score_edges, ComponentScores, Edge};
pub use emit::emit;
pub use evidence::{build_pair_evidence, EvidenceRecord, PairEvidenceMap, PairKey, PairState, SessionParticipants, Technique};
pub use history::{load_git_history, pair_history_score, CommitChanges, HistoryIndex};
pub use locator::{attach_locators, prior_context_atoms, Atom};
pub use op_stream::{build_op_stream, Op, OpKind, SessionRecord};
pub use participants::{merge_ranges_per_file, participants as build_participants, Participant, ParticipantKind};

use crate::advice::candidates::CandidateInput;
use crate::advice::detector::Detector;
use crate::advice::suggestion::Suggestion;

// ── Config ───────────────────────────────────────────────────────────────────

/// Configuration for the suggest detector.
///
/// Default values match the v4 constants in `docs/analyze-v4.mjs` lines 35–77.
#[derive(Clone, Debug)]
pub struct SuggestConfig {
    /// Enable trigram-similarity scoring channel (default: `true`).
    pub trigram_enabled: bool,
    /// Enable git-history co-edit scoring channel (default: `true`).
    pub history_enabled: bool,

    // op-stream
    pub window_ops: u32,
    pub locator_window: u32,
    pub locator_dir_penalty: f64,
    pub locator_prior_context_k: u32,
    pub range_merge_tolerance: u32,
    pub range_overlap_iou: f64,
    pub tree_diff_burst: u32,
    pub edit_weight_bump: f64,

    // scoring + viability
    pub max_same_file_dominance: f64,
    pub sprawl_op_distance_avg: u32,
    pub pair_cohesion_floor: f64,
    pub clique_cohesion_floor: f64,
    pub pair_escape_bonus: f64,
    pub edge_score_floor: f64,
    pub max_clique_size: u32,

    // history
    pub history_recency_commits: u32,
    pub history_half_life_commits: u32,
    pub history_saturation: u32,
    pub history_mass_refactor_default: u32,

    // IDF / shared-identifier
    pub shared_id_saturation: u32,

    // output
    pub top_n: u32,
    pub min_score: f64,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            trigram_enabled: true,
            history_enabled: true,

            // op-stream (analyze-v4.mjs lines 44–51)
            window_ops: 5,
            locator_window: 6,
            locator_dir_penalty: 0.4,
            locator_prior_context_k: 4,
            range_merge_tolerance: 5,
            range_overlap_iou: 0.30,
            tree_diff_burst: 3,
            edit_weight_bump: 1.25,

            // scoring + viability (analyze-v4.mjs lines 54–66)
            max_same_file_dominance: 0.66,
            sprawl_op_distance_avg: 4,
            pair_cohesion_floor: 0.30,
            clique_cohesion_floor: 0.30,
            pair_escape_bonus: 0.20,
            edge_score_floor: 0.40,
            max_clique_size: 8,

            // history (analyze-v4.mjs lines 69–72)
            history_recency_commits: 500,
            history_half_life_commits: 200,
            history_saturation: 4,
            history_mass_refactor_default: 12,

            // IDF / shared-identifier (analyze-v4.mjs line 75)
            shared_id_saturation: 6,

            // output (analyze-v4.mjs lines 38–39)
            top_n: 40,
            min_score: 0.0,
        }
    }
}

// ── Detector ─────────────────────────────────────────────────────────────────

/// No-op suggest detector. Returns an empty vec until the pipeline stages
/// are implemented (Steps 3+).
pub struct SuggestDetector {
    pub config: SuggestConfig,
}

impl SuggestDetector {
    pub fn new(config: SuggestConfig) -> Self {
        Self { config }
    }
}

impl Default for SuggestDetector {
    fn default() -> Self {
        Self::new(SuggestConfig::default())
    }
}

impl Detector for SuggestDetector {
    fn detect(&self, _input: &CandidateInput<'_>) -> Vec<Suggestion> {
        vec![]
    }
}
