//! Integration tests for the edge-scoring stage.

use git_mesh::advice::suggest::{
    HistoryIndex, Op, OpKind, Participant, ParticipantKind, SessionParticipants, SuggestConfig,
    build_canonical_ranges, build_pair_evidence, score_edges,
};

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

fn make_part(path: &str, start: u32, end: u32, sid: &str, op_index: usize) -> Participant {
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
        session_sid: sid.to_string(),
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
// Multi-session pair appears as shared_sessions > 1
// ---------------------------------------------------------------------------

#[test]
fn multi_session_pair_has_shared_sessions_count() {
    let mk = |sid: &str| {
        vec![
            make_part("a.rs", 1, 20, sid, 0),
            make_part("b.rs", 1, 20, sid, 1),
        ]
    };
    let multi_all: Vec<_> = [mk("s1"), mk("s2")].concat();
    let multi_canonical = build_canonical_ranges(&multi_all, &cfg_zero_floor());
    let multi_sessions = vec![make_session("s1", mk("s1")), make_session("s2", mk("s2"))];
    let multi_pairs = build_pair_evidence(&multi_sessions, &multi_canonical, &cfg_zero_floor());
    let multi_edges = score_edges(
        &multi_pairs,
        &multi_canonical,
        &HistoryIndex::default(),
        &cfg_zero_floor(),
    );

    assert!(!multi_edges.is_empty());
    assert_eq!(
        multi_edges[0].shared_sessions, 2,
        "pair seen in two sessions should report shared_sessions=2"
    );
}
