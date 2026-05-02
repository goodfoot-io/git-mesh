//! Integration tests for the edge-scoring stage.

use git_mesh::advice::suggest::{
    HistoryIndex, Op, OpKind, Participant, ParticipantKind, SessionParticipants, SuggestConfig,
    build_canonical_ranges, build_pair_evidence, is_cross_cutting_path, score_edges,
};
use std::collections::{BTreeMap, BTreeSet};

fn cfg_zero_floor() -> SuggestConfig {
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

// ---------------------------------------------------------------------------
// Score is bounded in [0, 1]
// ---------------------------------------------------------------------------

#[test]
fn edge_score_is_in_unit_interval() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 1);
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &cfg_zero_floor());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());
    let history = HistoryIndex::default();
    let edges = score_edges(&pairs, &canonical, &history, &cfg_zero_floor());
    for e in &edges {
        assert!(
            e.score >= 0.0 && e.score <= 1.0,
            "score {} out of [0,1]",
            e.score
        );
    }
}

// ---------------------------------------------------------------------------
// Cohesion seam is None
// ---------------------------------------------------------------------------

#[test]
fn per_edge_cohesion_is_always_none() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 1);
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &cfg_zero_floor());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());
    let history = HistoryIndex::default();
    let edges = score_edges(&pairs, &canonical, &history, &cfg_zero_floor());
    assert!(!edges.is_empty());
    for e in &edges {
        assert!(
            e.per_edge_cohesion.is_none(),
            "cohesion seam must be None from edges stage"
        );
    }
}

// ---------------------------------------------------------------------------
// Same-file pairs excluded
// ---------------------------------------------------------------------------

#[test]
fn same_file_pairs_excluded() {
    let p_a = make_part("a.rs", 1, 10, "s1", 0);
    let p_b = make_part("a.rs", 20, 30, "s1", 1); // same file, non-overlapping ranges
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &cfg_zero_floor());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());
    let history = HistoryIndex::default();
    let edges = score_edges(&pairs, &canonical, &history, &cfg_zero_floor());
    assert!(edges.is_empty(), "same-file pairs must not produce edges");
}

// ---------------------------------------------------------------------------
// Edge floor filters
// ---------------------------------------------------------------------------

#[test]
fn high_floor_removes_low_scoring_edges() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 5); // max distance from window
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &SuggestConfig::default());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &SuggestConfig::default());
    let history = HistoryIndex::default();
    let high_cfg = SuggestConfig {
        edge_score_floor: 0.99,
        ..SuggestConfig::default()
    };
    let edges = score_edges(&pairs, &canonical, &history, &high_cfg);
    assert!(edges.is_empty(), "nothing should pass a 0.99 floor");
}

// ---------------------------------------------------------------------------
// Pair shared_sessions is 1 — under single-session input there is exactly one
// observed session contributing to every edge. Hardcoding 0 silently zeroed
// the composite's `0.10 * sessions/3` term.
// ---------------------------------------------------------------------------

#[test]
fn pair_shared_sessions_is_one() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 1);
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &cfg_zero_floor());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());
    let edges = score_edges(&pairs, &canonical, &HistoryIndex::default(), &cfg_zero_floor());

    assert!(!edges.is_empty());
    assert_eq!(
        edges[0].shared_sessions, 1,
        "single-session input must report shared_sessions = 1"
    );
}

// ---------------------------------------------------------------------------
// Cross-cutting path filter — lockfile pair must NOT reach the edge set, so
// it can never ride the synthetic `historical-cochange` channel into High band.
// ---------------------------------------------------------------------------

#[test]
fn cross_cutting_helper_classifies_lockfiles_and_source() {
    assert!(is_cross_cutting_path("Cargo.lock"));
    assert!(is_cross_cutting_path("yarn.lock"));
    assert!(is_cross_cutting_path("node_modules/foo/index.js"));
    assert!(is_cross_cutting_path("dist/bundle.js"));
    assert!(is_cross_cutting_path(".gitignore"));
    assert!(!is_cross_cutting_path("src/main.rs"));
    assert!(!is_cross_cutting_path("Cargo.toml"));
    assert!(!is_cross_cutting_path("package.json"));
}

#[test]
fn lockfile_pair_with_strong_history_is_dropped_from_edges() {
    // Build a `Cargo.toml` ↔ `Cargo.lock` candidate pair with synthetic
    // history that clears the band's `historical_pair_commits >= 2` floor.
    // The pair must be excluded at edge construction so it cannot surface as
    // High band against single-session input.
    let p_a = make_part("Cargo.toml", 1, 20, "s1", 0);
    let p_b = make_part("Cargo.lock", 1, 20, "s1", 1);
    let all = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all, &cfg_zero_floor());
    let sessions = vec![make_session("s1", all)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());

    // Synthetic history: 5 shared commits between the two paths.
    let mut commits_by_path: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut commit_weight: BTreeMap<String, f64> = BTreeMap::new();
    let mut shared: BTreeSet<String> = BTreeSet::new();
    for i in 0..5 {
        let h = format!("c{i}");
        shared.insert(h.clone());
        commit_weight.insert(h, 1.0);
    }
    commits_by_path.insert("Cargo.toml".to_string(), shared.clone());
    commits_by_path.insert("Cargo.lock".to_string(), shared);
    let history = HistoryIndex {
        available: true,
        commits_by_path,
        commit_weight,
        total_commits: 5,
        mass_refactor_cap: 12,
    };

    let edges = score_edges(&pairs, &canonical, &history, &cfg_zero_floor());
    assert!(
        edges.is_empty(),
        "lockfile pair with strong synthetic history must be excluded from the edge set"
    );
}

// ---------------------------------------------------------------------------
// Component breakdown no longer carries `s_codensity` — the redundant term
// was dropped from the composite and its weight redistributed.
// ---------------------------------------------------------------------------

#[test]
fn component_scores_have_no_codensity_field() {
    let parts = vec![
        make_part("a.rs", 1, 20, "s1", 0),
        make_part("b.rs", 1, 20, "s1", 1),
    ];
    let canonical = build_canonical_ranges(&parts, &cfg_zero_floor());
    let sessions = vec![make_session("s1", parts)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg_zero_floor());
    let edges = score_edges(&pairs, &canonical, &HistoryIndex::default(), &cfg_zero_floor());
    assert!(!edges.is_empty());
    let c = &edges[0].components;
    // All five surviving fields must be in [0,1].
    for v in [c.s_cofreq, c.s_distance, c.s_edit, c.s_kind, c.s_history] {
        assert!((0.0..=1.0).contains(&v));
    }
}
