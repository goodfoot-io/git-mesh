//! Edge scoring stage (Section 10 of analyze-v4.mjs).
//!
//! Assembles a composite score for each pair in the `PairEvidenceMap` using
//! the history and evidence channels. Per-edge cohesion is a `None`
//! seam — the cohesion module (Step 3c substep 9) fills it in.

use crate::advice::suggest::SuggestConfig;
use crate::advice::suggest::canonical::CanonicalIndex;
use crate::advice::suggest::evidence::{PairEvidenceMap, Technique};
use crate::advice::suggest::history::{HistoryIndex, pair_history_score};

// ── Public types ──────────────────────────────────────────────────────────────

/// Component score breakdown (each in [0,1]).
#[derive(Clone, Debug)]
pub struct ComponentScores {
    pub s_cofreq: f64,
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

// ── Cross-cutting path filter ────────────────────────────────────────────────

/// Lockfiles, generated/build directories, and hidden top-level configs that
/// historically churn with everything in the repo. When a pair has either side
/// matching this filter, the pair is excluded from edge construction so the
/// "trivial co-change" exit (e.g. `Cargo.toml` ↔ `Cargo.lock`) cannot bypass
/// the band's channel-count cap by riding the synthetic `historical-cochange`
/// channel into High band.
///
/// Basename comparison is exact and case-sensitive. Directory-segment
/// comparison checks for `/<name>/` segments anywhere in the path.
pub fn is_cross_cutting_path(path: &str) -> bool {
    // Exact basename matches: lockfiles + hidden top-level configs that
    // historically churn with everything.
    const CROSS_CUTTING_BASENAMES: &[&str] = &[
        "Cargo.lock",
        "yarn.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "Gemfile.lock",
        "composer.lock",
        "poetry.lock",
        "uv.lock",
        "go.sum",
        "Pipfile.lock",
        ".gitignore",
        ".gitattributes",
    ];
    let basename = path.rsplit('/').next().unwrap_or(path);
    if CROSS_CUTTING_BASENAMES.contains(&basename) {
        return true;
    }

    // Path-component matches: anything inside a generated/build/vendored
    // directory tree.
    const CROSS_CUTTING_SEGMENTS: &[&str] = &[
        "/dist/",
        "/build/",
        "/generated/",
        "/.next/",
        "/node_modules/",
    ];
    // Normalize so a leading segment (e.g. `dist/foo`) is also caught.
    let normalized = format!("/{path}");
    for seg in CROSS_CUTTING_SEGMENTS {
        if normalized.contains(seg) {
            return true;
        }
    }

    false
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Score all pairs and return edges above `edge_score_floor`.
///
/// Ports `scoreEdges` from `docs/analyze-v4.mjs` line 561.
pub fn score_edges(
    pairs: &PairEvidenceMap,
    canonical: &CanonicalIndex,
    history: &HistoryIndex,
    cfg: &SuggestConfig,
) -> Vec<Edge> {
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
        // Skip pairs where either side is a cross-cutting path (lockfiles,
        // generated/build dirs, hidden top-level configs). These churn with
        // everything in the repo, so their `historical_pair_commits` clears
        // the band cap's `>= 2` floor trivially and would surface non-semantic
        // dependencies (e.g. `Cargo.toml` ↔ `Cargo.lock`) as High band.
        if is_cross_cutting_path(&a_range.path) || is_cross_cutting_path(&b_range.path) {
            continue;
        }

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

        let (hist_count, hist_weighted) = pair_history_score(history, &a_range.path, &b_range.path);

        // Component scores in [0,1].
        let window_ops = cfg.window_ops as f64;
        let total_commits = history.total_commits.max(1);
        let s_cofreq = (hist_count as f64 / total_commits as f64).min(1.0);
        let s_distance = 1.0 - (mean_distance.min(window_ops) / (window_ops + 1.0));
        let s_edit = (pair.edit_hits as f64 / 3.0).min(1.0);
        let s_kind = (pair.kinds.len() as f64 / 4.0).min(1.0);
        let s_history = if history.available {
            (hist_weighted / cfg.history_saturation as f64).min(1.0)
        } else {
            0.5
        };

        // Weighted composite (weights sum to 0.88; 0.12 reserved for cohesion).
        // Compared to the original analyze-v4.mjs port, `s_codensity` is removed
        // (it was a duplicate `hist_weighted / total_commits` term that
        // triple-counted history alongside `s_cofreq` and `s_history`). Its
        // 0.14 weight is redistributed: +0.08 to `s_history` (recency-weighted
        // density is the better history signal) and +0.02 each to
        // `s_distance`/`s_edit`/`s_kind`. Sum: 0.18 + 0.16 + 0.14 + 0.12 + 0.28
        // = 0.88, matching the original envelope.
        let score = 0.18 * s_cofreq
            + 0.16 * s_distance
            + 0.14 * s_edit
            + 0.12 * s_kind
            + 0.28 * s_history;

        if score < cfg.edge_score_floor {
            continue;
        }

        let mut kinds_sorted: Vec<String> = pair
            .kinds
            .iter()
            .map(|k| match k {
                Technique::OperationWindow => "operation-window".to_string(),
                Technique::LocatorEditContext => "locator-edit-context".to_string(),
            })
            .collect();
        // Historical co-change is surfaced in `kinds` so downstream
        // diagnostics can see it, but `confidence_band` excludes it from the
        // channel-count cap (see `band::in_session_technique_count`). The cap
        // is meant to count *in-session* evidence channels; counting history
        // as a distinct technique lets a single-touch single-session run with
        // one historical co-change reach High trivially, masking the
        // "one channel caps at Medium" invariant. History is already
        // weighted into the composite via `s_history` and `s_cofreq`.
        if hist_count > 0 {
            kinds_sorted.push("historical-cochange".to_string());
        }
        kinds_sorted.sort();

        edges.push(Edge {
            canonical_a: a,
            canonical_b: b,
            score,
            components: ComponentScores {
                s_cofreq,
                s_distance,
                s_edit,
                s_kind,
                s_history,
            },
            per_edge_cohesion: None,
            // Single-session input: one observed session contributes to every
            // edge by definition. Hardcoding 0 silently zeroed the
            // `0.10 * sessions/3` composite term and demoted real suggestions.
            shared_sessions: 1,
            mean_op_distance: mean_distance,
            lift: 0.0,
            confidence: 0.0,
            support: 0.0,
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
    use crate::advice::suggest::SuggestConfig;
    use crate::advice::suggest::canonical::build_canonical_ranges;
    use crate::advice::suggest::evidence::{SessionParticipants, build_pair_evidence};
    use crate::advice::suggest::history::HistoryIndex;
    use crate::advice::suggest::op_stream::{Op, OpKind};
    use crate::advice::suggest::participants::{Participant, ParticipantKind};
    use std::collections::BTreeSet;

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

    fn make_part(path: &str, start: u32, end: u32, _sid: &str, op_index: usize) -> Participant {
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
    fn cross_cutting_path_helper_classifies_lockfiles_and_build_dirs() {
        // Cross-cutting (true).
        assert!(is_cross_cutting_path("Cargo.lock"));
        assert!(is_cross_cutting_path("yarn.lock"));
        assert!(is_cross_cutting_path("packages/foo/yarn.lock"));
        assert!(is_cross_cutting_path("node_modules/foo/index.js"));
        assert!(is_cross_cutting_path("packages/x/dist/bundle.js"));
        assert!(is_cross_cutting_path("dist/bundle.js"));
        assert!(is_cross_cutting_path(".gitignore"));
        assert!(is_cross_cutting_path("packages/x/build/out.o"));
        assert!(is_cross_cutting_path("apps/web/.next/server/page.js"));

        // Source files (false).
        assert!(!is_cross_cutting_path("src/main.rs"));
        assert!(!is_cross_cutting_path("Cargo.toml"));
        assert!(!is_cross_cutting_path("package.json"));
        // Case-sensitive: lowercase variant must not match.
        assert!(!is_cross_cutting_path("cargo.lock"));
    }

    #[test]
    fn lockfile_pair_is_excluded_from_edges() {
        // `Cargo.toml` ↔ `Cargo.lock` is the canonical trivial-co-change exit.
        // The pair must be dropped at edge construction so neither the
        // cross-channel score nor the historical-cochange channel can carry
        // it into the candidate set.
        let p_a = make_part("Cargo.toml", 1, 20, "s1", 0);
        let p_b = make_part("Cargo.lock", 1, 20, "s1", 1);
        let all = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all, &cfg());
        let sessions = vec![make_session("s1", all)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        // Even with strong synthetic history, the pair must be dropped.
        let mut history = HistoryIndex {
            available: true,
            ..HistoryIndex::default()
        };
        history.total_commits = 10;
        let mut commits: BTreeSet<String> = BTreeSet::new();
        for i in 0..5 {
            commits.insert(format!("c{i}"));
            history.commit_weight.insert(format!("c{i}"), 1.0);
        }
        history
            .commits_by_path
            .insert("Cargo.toml".to_string(), commits.clone());
        history
            .commits_by_path
            .insert("Cargo.lock".to_string(), commits);
        let edges = score_edges(&pairs, &canonical, &history, &cfg());
        assert!(
            edges.is_empty(),
            "lockfile pair must be excluded from edge construction"
        );
    }

    #[test]
    fn score_edges_produces_edge_for_cross_file_pair() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        let history = HistoryIndex::default();

        let edges = score_edges(
            &pairs,
            &canonical,
            &history,
            &cfg(),
        );
        assert_eq!(edges.len(), 1, "should produce one edge");
        let edge = &edges[0];
        assert!(edge.score >= 0.0);
        assert!(
            edge.per_edge_cohesion.is_none(),
            "cohesion seam must be None"
        );
    }

    #[test]
    fn same_file_pair_is_excluded() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("a.rs", 30, 50, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        let history = HistoryIndex::default();

        let edges = score_edges(
            &pairs,
            &canonical,
            &history,
            &cfg(),
        );
        assert!(edges.is_empty(), "same-file pairs must be excluded");
    }

    #[test]
    fn edge_score_floor_filters_low_scores() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 4); // distance = 4, max window = 5
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());

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
            &canonical,
            &history,
            &high_floor_cfg,
        );
        assert!(edges.is_empty(), "score below floor must be excluded");
    }
}
