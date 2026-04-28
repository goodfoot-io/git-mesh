//! Candidate composite scoring (Section 13 of analyze-v4.mjs) and the
//! cohesion gate (Section 14).
//!
//! `score_candidate` assembles the four-granularity cohesion values and the
//! per-channel breakdown into a `CandidateScore`. `passes_cohesion_gate`
//! applies the v4 gate logic.

use std::collections::BTreeSet;

use crate::advice::suggest::SuggestConfig;
use crate::advice::suggest::canonical::CanonicalIndex;
use crate::advice::suggest::cliques::{Adjacency, edges_within};
use crate::advice::suggest::cohesion::{
    CanonicalId, SourceCache, intersection_cohesion, pairwise_cohesion_stats, trigram_cohesion,
};
use crate::advice::suggest::history::HistoryIndex;

// ── Public types ──────────────────────────────────────────────────────────────

/// Per-component score breakdown for a candidate clique.
#[derive(Clone, Debug)]
pub struct ComponentBreakdown {
    pub mean_edge_score: f64,
    pub density: f64,
    pub diversity_factor: f64,
    pub edit_hits: u32,
    pub trigram_score: f64,
    pub intersection_cohesion: f64,
    pub pairwise_min_cohesion: f64,
    pub pairwise_median_cohesion: f64,
    pub pairwise_mean_cohesion: f64,
    pub cluster_cohesion: f64,
    pub history_score: f64,
}

/// Fully-scored candidate clique ready for gate-checking and band assignment.
#[derive(Clone, Debug)]
pub struct CandidateScore {
    /// Sorted constituent canonical ids.
    pub canon_ids: Vec<CanonicalId>,
    /// Number of ranges in the clique.
    pub size: usize,
    pub distinct_files: usize,
    pub sessions: usize,
    pub components: ComponentBreakdown,
    /// Unique technique strings used by in-edges, sorted.
    pub techniques: Vec<String>,
    /// Average history co-edit count across in-edges.
    pub historical_pair_commits: u32,
    /// Average history weighted score.
    pub historical_weighted: f64,
    /// Max share of ranges from a single file (in [0,1]).
    pub same_file_dominance: f64,
    pub cross_package: bool,
    pub op_distance_avg: f64,
    /// Display tokens for shared identifier explanation.
    pub shared_identifiers: Vec<String>,
    /// Composite score in [0,1].
    pub composite: f64,
}

// ── Public API ────────────────────────────────────────────────────────────────

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Score a candidate clique.
///
/// Ports `scoreCandidate` from `docs/analyze-v4.mjs` line 825.
///
/// Note: compared to the JS, this takes an explicit `edges` slice because the
/// Rust `Adjacency` stores edge *indices* (not edge objects) to avoid cloning.
/// The step 3d orchestrator passes the same slice used to build `adj`.
#[allow(clippy::too_many_arguments)]
pub fn score_candidate(
    canon_ids: &[CanonicalId],
    edges: &[crate::advice::suggest::edges::Edge],
    adj: &Adjacency,
    canonical: &CanonicalIndex,
    source_cache: &SourceCache,
    idf: &crate::advice::suggest::cohesion::Idf,
    history: &HistoryIndex,
    cfg: &SuggestConfig,
) -> CandidateScore {
    let in_edge_refs = edges_within(canon_ids, adj);
    let in_edges: Vec<&crate::advice::suggest::edges::Edge> =
        in_edge_refs.iter().map(|&i| &edges[i]).collect();

    let pair_count = (canon_ids.len() * (canon_ids.len() - 1)) / 2;
    let density = in_edges.len() as f64 / pair_count.max(1) as f64;

    let sessions: usize = {
        let mut all: BTreeSet<usize> = BTreeSet::new();
        for e in &in_edges {
            all.insert(e.shared_sessions);
        }
        // JS: sessionsAll is a set of session IDs across all edges' shared_sessions.
        // In Rust, Edge::shared_sessions is a count, not a set of IDs.
        // We use the raw count from the edges (sum of distinct sessions per edge,
        // deduplication is approximate). Use max shared_sessions as a proxy for
        // distinct sessions (the JS set-union is lossy in the Rust representation).
        // More accurate: use the maximum shared_sessions value across in-edges since
        // the JS collects `e.shared_sessions` (which is a Set in the JS) per edge.
        // In Rust, Edge::shared_sessions is already a count (usize). We take the max
        // as the closest approximation without propagating the session-ID set.
        in_edges
            .iter()
            .map(|e| e.shared_sessions)
            .max()
            .unwrap_or(0)
    };

    let mean_edge_score = if in_edges.is_empty() {
        0.0
    } else {
        in_edges.iter().map(|e| e.score).sum::<f64>() / in_edges.len() as f64
    };
    let mean_op_distance = if in_edges.is_empty() {
        cfg.window_ops as f64
    } else {
        in_edges.iter().map(|e| e.mean_op_distance).sum::<f64>() / in_edges.len() as f64
    };
    let edit_hits: u32 = in_edges.iter().map(|e| e.edit_hits).sum();
    let techniques: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for e in &in_edges {
            for k in &e.kinds {
                set.insert(k.clone());
            }
        }
        set.into_iter().collect()
    };

    // File diversity.
    let ranges: Vec<_> = canon_ids
        .iter()
        .filter_map(|&id| canonical.ranges.get(id))
        .collect();
    let mut file_counts: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for r in &ranges {
        *file_counts.entry(r.path.as_str()).or_default() += 1;
    }
    let distinct_files = file_counts.len();
    let max_path_share =
        file_counts.values().copied().max().unwrap_or(0) as f64 / ranges.len().max(1) as f64;
    let top_dirs: BTreeSet<String> = ranges
        .iter()
        .map(|r| {
            let parts: Vec<&str> = r.path.splitn(4, '/').collect();
            parts[..parts.len().min(3)].join("/")
        })
        .collect();
    let cross_package = top_dirs.len() >= 2;

    // Content cohesion (four granularities).
    let all_in_cache = canon_ids.iter().all(|&id| {
        canonical.ranges.get(id).is_some_and(|r| {
            let key = format!("{}#{}-{}", r.path, r.start, r.end);
            source_cache.contains_key(&key)
        })
    });

    let (trigram, intersect_weight, intersect_tokens, pw) = if all_in_cache {
        // Trigram cohesion.
        let trigram_map: std::collections::BTreeMap<CanonicalId, BTreeSet<String>> = canon_ids
            .iter()
            .filter_map(|&id| {
                let r = canonical.ranges.get(id)?;
                let key = format!("{}#{}-{}", r.path, r.start, r.end);
                let rt = source_cache.get(&key)?;
                Some((id, rt.trigrams.clone()))
            })
            .collect();
        let trig = trigram_cohesion(&trigram_map);

        let (iw, it) = intersection_cohesion(
            canon_ids,
            source_cache,
            canonical,
            idf,
            cfg.shared_id_saturation,
        );
        let pw = pairwise_cohesion_stats(
            canon_ids,
            source_cache,
            canonical,
            idf,
            cfg.shared_id_saturation,
        );
        (trig, iw, it, pw)
    } else {
        let zero =
            pairwise_cohesion_stats(&[], source_cache, canonical, idf, cfg.shared_id_saturation);
        (0.0, 0.0, vec![], zero)
    };

    let cluster_cohesion = intersect_weight.max(pw.median).max(pw.mean);
    // Display tokens: prefer intersection tokens; fallback to weakest-pair tokens.
    let display_tokens = if !intersect_tokens.is_empty() {
        intersect_tokens
    } else if let Some([a, b]) = pw.weakest_pair {
        // Recompute pair tokens for display.
        if let (Some(ra), Some(rb)) = (canonical.ranges.get(a), canonical.ranges.get(b)) {
            let ka = format!("{}#{}-{}", ra.path, ra.start, ra.end);
            let kb = format!("{}#{}-{}", rb.path, rb.start, rb.end);
            if let (Some(ta), Some(tb)) = (source_cache.get(&ka), source_cache.get(&kb)) {
                let inter: Vec<&str> = ta
                    .identifiers
                    .iter()
                    .filter(|t| t.len() >= 4 && tb.identifiers.contains(*t))
                    .map(|t| t.as_str())
                    .collect();
                let mut ranked: Vec<(&str, f64)> = inter
                    .into_iter()
                    .map(|t| (t, *idf.get(t).unwrap_or(&0.0)))
                    .collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                ranked.iter().take(8).map(|(t, _)| t.to_string()).collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    // History score.
    let hist_avg = if in_edges.is_empty() {
        0.0
    } else {
        in_edges.iter().map(|e| e.history_weighted).sum::<f64>() / in_edges.len() as f64
    };
    let hist_count = if in_edges.is_empty() {
        0
    } else {
        (in_edges
            .iter()
            .map(|e| e.history_pair_commits as f64)
            .sum::<f64>()
            / in_edges.len() as f64)
            .round() as u32
    };
    let s_history = if history.available {
        (hist_avg / cfg.history_saturation as f64).min(1.0)
    } else {
        0.5
    };

    // Composite weights (sum to 1.0 per the v4 plan).
    let diversity_factor = (distinct_files as f64 / ranges.len().max(1) as f64)
        * if cross_package { 1.0 } else { 0.9 };
    let composite = 0.18 * mean_edge_score
        + 0.10 * density
        + 0.10 * (sessions as f64).min(3.0) / 3.0
        + 0.08 * diversity_factor
        + 0.08 * (edit_hits as f64).min(4.0) / 4.0
        + 0.06 * (techniques.len() as f64).min(5.0) / 5.0
        + 0.10 * trigram
        + 0.10 * cluster_cohesion
        + 0.10 * s_history
        + 0.10 * mean_edge_score.min(1.0);

    let mut sorted_ids = canon_ids.to_vec();
    sorted_ids.sort_unstable();

    CandidateScore {
        canon_ids: sorted_ids,
        size: ranges.len(),
        distinct_files,
        sessions,
        components: ComponentBreakdown {
            mean_edge_score: round3(mean_edge_score),
            density: round3(density),
            diversity_factor: round3(diversity_factor),
            edit_hits,
            trigram_score: round3(trigram),
            intersection_cohesion: round3(intersect_weight),
            pairwise_min_cohesion: round3(pw.min),
            pairwise_median_cohesion: round3(pw.median),
            pairwise_mean_cohesion: round3(pw.mean),
            cluster_cohesion: round3(cluster_cohesion),
            history_score: round3(s_history),
        },
        techniques,
        historical_pair_commits: hist_count,
        historical_weighted: round3(hist_avg),
        same_file_dominance: round3(max_path_share),
        cross_package,
        op_distance_avg: round3(mean_op_distance),
        shared_identifiers: display_tokens,
        composite: round3(composite),
    }
}

/// Cohesion gate for a candidate clique.
///
/// Ports `passesCohesionGate` from `docs/analyze-v4.mjs` line 941.
pub fn passes_cohesion_gate(c: &CandidateScore, cfg: &SuggestConfig) -> bool {
    if c.size == 2 {
        return true; // pairs gated by edge-score floor only
    }
    // Hard floor: no zero-cohesion pair.
    if c.components.pairwise_min_cohesion < 0.10 {
        return false;
    }
    if c.components.intersection_cohesion >= cfg.clique_cohesion_floor {
        return true;
    }
    if c.components.pairwise_median_cohesion >= cfg.clique_cohesion_floor + 0.10 {
        return true;
    }
    if c.components.pairwise_min_cohesion >= cfg.clique_cohesion_floor + 0.20 {
        return true;
    }
    if c.components.trigram_score >= 0.20 {
        return true;
    }
    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::SuggestConfig;

    fn cfg() -> SuggestConfig {
        SuggestConfig::default()
    }

    fn make_candidate(
        size: usize,
        pairwise_min: f64,
        intersection: f64,
        pairwise_median: f64,
        trigram: f64,
    ) -> CandidateScore {
        CandidateScore {
            canon_ids: (0..size).collect(),
            size,
            distinct_files: size,
            sessions: 1,
            components: ComponentBreakdown {
                mean_edge_score: 0.5,
                density: 1.0,
                diversity_factor: 1.0,
                edit_hits: 1,
                trigram_score: trigram,
                intersection_cohesion: intersection,
                pairwise_min_cohesion: pairwise_min,
                pairwise_median_cohesion: pairwise_median,
                pairwise_mean_cohesion: pairwise_median,
                cluster_cohesion: intersection.max(pairwise_median),
                history_score: 0.5,
            },
            techniques: vec!["operation-window".to_string()],
            historical_pair_commits: 0,
            historical_weighted: 0.0,
            same_file_dominance: 1.0 / size as f64,
            cross_package: true,
            op_distance_avg: 2.0,
            shared_identifiers: vec![],
            composite: 0.55,
        }
    }

    #[test]
    fn pair_always_passes_cohesion_gate() {
        let c = make_candidate(2, 0.0, 0.0, 0.0, 0.0);
        assert!(passes_cohesion_gate(&c, &cfg()));
    }

    #[test]
    fn clique_with_zero_pairwise_min_fails_gate() {
        let c = make_candidate(3, 0.0, 0.50, 0.50, 0.50);
        assert!(!passes_cohesion_gate(&c, &cfg()));
    }

    #[test]
    fn clique_with_high_intersection_passes_gate() {
        // pairwise_min >= 0.10, intersection >= clique_cohesion_floor (0.30)
        let c = make_candidate(3, 0.15, 0.35, 0.20, 0.10);
        assert!(passes_cohesion_gate(&c, &cfg()));
    }

    #[test]
    fn clique_with_high_trigram_passes_gate() {
        let c = make_candidate(3, 0.15, 0.10, 0.10, 0.25);
        assert!(passes_cohesion_gate(&c, &cfg()));
    }

    #[test]
    fn clique_failing_all_gates_returns_false() {
        // pairwise_min=0.15 (≥0.10), but everything else below threshold.
        let c = make_candidate(3, 0.15, 0.05, 0.10, 0.10);
        assert!(!passes_cohesion_gate(&c, &cfg()));
    }
}
