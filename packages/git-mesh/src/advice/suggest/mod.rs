//! n-ary mesh suggestion detector.
//!
//! `SuggestDetector` implements the `Detector` trait and runs the full v4
//! pipeline (trigram scoring, history channel, clique enumeration).
//! The primary entry point for parity testing is `run_suggest_pipeline`,
//! which accepts pre-loaded `SessionRecord`s and an optional git repo.

pub(crate) mod history_cache;
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

pub use band::{confidence_band, viability_label};
pub use canonical::{CanonicalIndex, CanonicalRange, build_canonical_ranges, range_iou};
pub use cliques::{
    Adjacency, bron_kerbosch, build_edge_adjacency, connected_components, edges_within,
};
pub use cohesion::{
    CanonicalId, Idf, RangeTokens, SourceCache, build_idf, cache_range, jaccard, per_edge_cohesion,
    range_tokens_of, read_anchor, tokens_of, trigram_cohesion, trigrams_of,
};
pub use composite::{CandidateScore, ComponentBreakdown, passes_cohesion_gate, score_candidate};
pub use edges::{ComponentScores, Edge, score_edges};
pub use emit::emit;
pub use evidence::{
    EvidenceRecord, PairEvidenceMap, PairKey, PairState, SessionParticipants, Technique,
    build_pair_evidence,
};
pub use history::{CommitChanges, HistoryIndex, load_git_history, pair_history_score};
pub use locator::{Atom, attach_locators, prior_context_atoms};
pub use op_stream::{Op, OpKind, SessionRecord, build_op_stream};
pub use participants::{
    Participant, ParticipantKind, merge_ranges_per_file, participants as build_participants,
};

use std::collections::BTreeMap;
use std::path::Path;

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

impl SuggestConfig {
    /// Build a `SuggestConfig` from the process environment.
    ///
    /// Honoured variables:
    /// - `GIT_MESH_SUGGEST_TRIGRAM=0`  → `trigram_enabled = false`
    /// - `GIT_MESH_SUGGEST_HISTORY=0`  → `history_enabled = false`
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if std::env::var("GIT_MESH_SUGGEST_TRIGRAM").as_deref() == Ok("0") {
            cfg.trigram_enabled = false;
        }
        if std::env::var("GIT_MESH_SUGGEST_HISTORY").as_deref() == Ok("0") {
            cfg.history_enabled = false;
        }
        cfg
    }
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

/// Run the full v4 suggest pipeline and return scored `Suggestion`s.
///
/// `sessions`    — pre-loaded session records (reads + touches).
/// `repo`        — optional git repository for the history channel.
/// `repo_root`   — filesystem root used for file I/O in the cohesion stage.
///                 Pass the worktree root when a real repo is available,
///                 or any path when history is disabled (cohesion reads will
///                 silently return empty tokens).
/// `session_dir` — optional path to the session directory used to cache the
///                 git history walk across repeated flushes. Pass `None` when
///                 no session-local cache is desired (e.g. corpus-wide runs).
///
/// This is the primary entry point for parity testing and the CLI
/// `git mesh advice <id> suggest` subcommand.
pub fn run_suggest_pipeline(
    sessions: &[SessionRecord],
    repo: Option<&gix::Repository>,
    repo_root: &Path,
    cfg: &SuggestConfig,
    session_dir: Option<&Path>,
) -> Vec<Suggestion> {
    if sessions.is_empty() {
        return Vec::new();
    }

    // ── Stages 3–5: op-stream → locators → participants per session ───────────
    let mut session_participants: Vec<SessionParticipants> = Vec::new();
    let mut all_parts: Vec<Participant> = Vec::new();

    for rec in sessions {
        let mut ops = build_op_stream(rec, cfg);
        attach_locators(&mut ops, cfg);
        let raw_parts = build_participants(&ops, &rec.sid);
        let merged = merge_ranges_per_file(&raw_parts, cfg);
        all_parts.extend(merged.clone());
        session_participants.push(SessionParticipants {
            sid: rec.sid.clone(),
            ops,
            parts: merged,
        });
    }

    // ── Stage 6: canonical ranges ─────────────────────────────────────────────
    let canonical = build_canonical_ranges(&all_parts, cfg);
    if canonical.ranges.is_empty() {
        return Vec::new();
    }

    // ── Stage 7: pair evidence ────────────────────────────────────────────────
    let pairs = build_pair_evidence(&session_participants, &canonical, cfg);
    if pairs.is_empty() {
        return Vec::new();
    }

    // ── Stage 8: git history ──────────────────────────────────────────────────
    let mut effective_cfg = cfg.clone();
    let history = if cfg.history_enabled {
        if let Some(r) = repo {
            let all_paths: Vec<String> = canonical.ranges.iter().map(|r| r.path.clone()).collect();
            match load_git_history(r, &all_paths, cfg, session_dir) {
                Ok(h) => h,
                Err(_) => {
                    effective_cfg.history_enabled = false;
                    HistoryIndex::default()
                }
            }
        } else {
            effective_cfg.history_enabled = false;
            HistoryIndex::default()
        }
    } else {
        HistoryIndex::default()
    };
    let history_available = history.available;

    // ── Stage 10: score edges (pre-content, no floor yet) ────────────────────
    // The JS applies the edge_score_floor AFTER adding per-edge cohesion
    // (0.12 * pair_cohesion), not before. Pass floor=0 so score_edges returns
    // all edges; the real floor is enforced below after cohesion is added.
    let mut no_floor_cfg = effective_cfg.clone();
    no_floor_cfg.edge_score_floor = 0.0;
    let mut edges = score_edges(
        &pairs,
        &canonical,
        &history,
        &no_floor_cfg,
    );

    // ── Stage 11: per-edge cohesion (fill the None seam) ─────────────────────
    // Build source cache and IDF from canonical ranges if trigram is enabled.
    let mut source_cache: SourceCache = BTreeMap::new();
    let idf: Idf = if effective_cfg.trigram_enabled {
        // Populate the source cache for all canonical ranges.
        for r in &canonical.ranges {
            cache_range(repo_root, &r.path, r.start, r.end, &mut source_cache);
        }
        // Build IDF from token lists.
        let range_tokens_for_idf: BTreeMap<CanonicalId, Vec<String>> = canonical
            .ranges
            .iter()
            .enumerate()
            .filter_map(|(id, r)| {
                let key = format!("{}#{}-{}", r.path, r.start, r.end);
                source_cache
                    .get(&key)
                    .map(|rt| (id, rt.identifiers.iter().cloned().collect::<Vec<_>>()))
            })
            .collect();
        build_idf(&range_tokens_for_idf)
    } else {
        BTreeMap::new()
    };

    // Fill per_edge_cohesion for each edge.
    for edge in &mut edges {
        let a_range = match canonical.ranges.get(edge.canonical_a) {
            Some(r) => r,
            None => continue,
        };
        let b_range = match canonical.ranges.get(edge.canonical_b) {
            Some(r) => r,
            None => continue,
        };
        let key_a = format!("{}#{}-{}", a_range.path, a_range.start, a_range.end);
        let key_b = format!("{}#{}-{}", b_range.path, b_range.start, b_range.end);
        // Use None when files are unavailable (distinguishes "not computable"
        // from "computable but zero"). The edge-floor filter only applies the
        // pair_cohesion >= 0.10 gate when cohesion is Some (files exist).
        edge.per_edge_cohesion = match (source_cache.get(&key_a), source_cache.get(&key_b)) {
            (Some(ta), Some(tb)) => Some(per_edge_cohesion(
                ta,
                tb,
                &idf,
                effective_cfg.shared_id_saturation,
            )),
            _ => None,
        };
    }

    // ── Edge-floor pruning (post-cohesion, matching JS line 1093) ────────────
    // Final score = score_pre_content + 0.12 * pair_cohesion.
    // The JS also requires pair_cohesion >= 0.10 when trigram is enabled AND
    // the source files were accessible (`per_edge_cohesion = Some`). When
    // files are unavailable the cohesion gate is skipped (pair is kept on
    // behaviour evidence alone).
    for edge in &mut edges {
        let cohesion = edge.per_edge_cohesion.unwrap_or(0.0);
        edge.score += 0.12 * cohesion;
    }
    edges.retain(|e| {
        if e.score < effective_cfg.edge_score_floor {
            crate::advice_debug!(
                "suggester-edge",
                "a" => e.canonical_a,
                "b" => e.canonical_b,
                "action" => "dropped",
                "reason" => "score-floor",
                "score" => e.score
            );
            return false;
        }
        // Trigram cohesion gate: only when trigram is enabled AND files were
        // readable. None = files absent → skip gate.
        if effective_cfg.trigram_enabled && e.per_edge_cohesion.is_some_and(|c| c < 0.10) {
            crate::advice_debug!(
                "suggester-edge",
                "a" => e.canonical_a,
                "b" => e.canonical_b,
                "action" => "dropped",
                "cohesion" => e.per_edge_cohesion.unwrap_or(0.0),
                "reason" => "cohesion-gate"
            );
            return false;
        }
        crate::advice_debug!(
            "suggester-edge",
            "a" => e.canonical_a,
            "b" => e.canonical_b,
            "action" => "kept",
            "cohesion" => e.per_edge_cohesion.unwrap_or(0.0),
            "score" => e.score
        );
        true
    });

    if edges.is_empty() {
        return Vec::new();
    }

    // ── Stage 12: clique enumeration ──────────────────────────────────────────
    let adj = build_edge_adjacency(&edges);
    let components = connected_components(&adj);
    let mut all_cliques: Vec<Vec<CanonicalId>> = Vec::new();
    for comp in &components {
        let cliques = bron_kerbosch(comp, &adj, effective_cfg.max_clique_size as usize);
        all_cliques.extend(cliques);
    }
    // Also add size-2 pairs (each edge is a potential pair candidate).
    for e in &edges {
        all_cliques.push(vec![e.canonical_a, e.canonical_b]);
    }
    // Deduplicate and sort for determinism.
    all_cliques.sort();
    all_cliques.dedup();

    // ── Stage 13: score candidates ────────────────────────────────────────────
    let scored: Vec<CandidateScore> = all_cliques
        .iter()
        .map(|ids| {
            score_candidate(
                ids,
                &edges,
                &adj,
                &canonical,
                &source_cache,
                &idf,
                &history,
                &effective_cfg,
            )
        })
        .collect();

    // ── Stage 14: pre-emission filters (JS lines 1124–1133) ──────────────────
    // Apply the same candidate-level filters the JS applies before `emit()`:
    //   1. composite >= MIN_SCORE (0.0 by default)
    //   2. distinct_files >= 2
    //   3. same_file_dominance <= MAX_SAME_FILE_DOMINANCE
    //   4. op_distance_avg <= SPRAWL_OP_DISTANCE_AVG
    //   5. viability != 'weak' (Suppressed in Rust)
    let scored: Vec<CandidateScore> = scored
        .into_iter()
        .filter(|c| {
            let ids_str = c.canon_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            if c.composite < effective_cfg.min_score {
                crate::advice_debug!("suggester-drop", "reason" => "min-score", "canonical_ids" => ids_str, "composite" => c.composite);
                return false;
            }
            if c.distinct_files < 2 {
                crate::advice_debug!("suggester-drop", "reason" => "distinct-files", "canonical_ids" => ids_str);
                return false;
            }
            if c.same_file_dominance > effective_cfg.max_same_file_dominance {
                crate::advice_debug!("suggester-drop", "reason" => "same-file-dominance", "canonical_ids" => ids_str, "dominance" => c.same_file_dominance);
                return false;
            }
            if c.op_distance_avg > effective_cfg.sprawl_op_distance_avg as f64 {
                crate::advice_debug!("suggester-drop", "reason" => "op-distance", "canonical_ids" => ids_str, "op_distance_avg" => c.op_distance_avg);
                return false;
            }
            // Pre-assign viability; drop weak candidates.
            let viability = viability_label(c, history_available);
            if viability == crate::advice::suggestion::Viability::Suppressed {
                crate::advice_debug!("suggester-drop", "reason" => "viability:Suppressed", "canonical_ids" => ids_str, "composite" => c.composite);
                return false;
            }
            true
        })
        .collect();

    // ── Emission ──────────────────────────────────────────────────────────────
    emit(scored, &canonical, &effective_cfg, history_available)
}

// ── Detector ─────────────────────────────────────────────────────────────────

/// Suggest detector. Runs the full v4 pipeline on sessions loaded from the
/// advice store associated with `CandidateInput`.
///
/// Note: `CandidateInput` does not carry pre-loaded sessions, so this
/// `Detector` implementation returns an empty vec. The primary entry point
/// for production use is `run_suggest_pipeline` (called by the CLI
/// `git mesh advice <id> suggest` subcommand).
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
    fn detect(&self, _input: &CandidateInput<'_>) -> anyhow::Result<Vec<Suggestion>> {
        // `CandidateInput` does not carry pre-loaded sessions or a repo reference.
        // The full pipeline is invoked via `run_suggest_pipeline` from the CLI
        // `git mesh advice suggest` subcommand. Wiring `SuggestDetector` into the
        // candidate aggregator without that input would silently produce nothing —
        // fail loudly so a future cutover discovers the missing seam instead.
        anyhow::bail!(
            "SuggestDetector requires sessions+repo input not carried by CandidateInput; \
             invoke run_suggest_pipeline directly"
        )
    }
}
