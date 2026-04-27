//! Integration tests for the pair-evidence stage.

use git_mesh::advice::suggest::{
    build_canonical_ranges, build_pair_evidence,
    Op, OpKind, Participant, ParticipantKind, SessionParticipants, SuggestConfig, Technique,
};

fn cfg() -> SuggestConfig {
    SuggestConfig::default()
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
// operation-window channel
// ---------------------------------------------------------------------------

#[test]
fn op_window_pair_within_distance_recorded() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 3); // distance 3, within window 5
    let all_parts = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![make_session("s1", all_parts)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
    assert_eq!(pairs.len(), 1);
    let state = pairs.values().next().unwrap();
    assert!(state.evidence.iter().any(|e| e.technique == Technique::OperationWindow));
}

#[test]
fn op_window_pair_at_distance_exactly_window_recorded() {
    // distance == window_ops (5) should still be included (boundary inclusive).
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 5); // distance == 5 == window_ops
    let all_parts = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![make_session("s1", all_parts)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
    assert_eq!(pairs.len(), 1, "distance == window must be included");
}

#[test]
fn op_window_pair_beyond_window_excluded() {
    let p_a = make_part("a.rs", 1, 20, "s1", 0);
    let p_b = make_part("b.rs", 1, 20, "s1", 6); // distance 6 > window 5
    let all_parts = vec![p_a.clone(), p_b.clone()];
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![make_session("s1", all_parts)];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
    assert!(pairs.is_empty(), "distance > window must be excluded");
}

// ---------------------------------------------------------------------------
// session-recurrence channel
// ---------------------------------------------------------------------------

#[test]
fn two_sessions_same_pair_produces_recurrence_evidence() {
    let p_a1 = make_part("a.rs", 1, 20, "s1", 0);
    let p_b1 = make_part("b.rs", 1, 20, "s1", 1);
    let p_a2 = make_part("a.rs", 1, 20, "s2", 0);
    let p_b2 = make_part("b.rs", 1, 20, "s2", 1);
    let all_parts = vec![p_a1.clone(), p_b1.clone(), p_a2.clone(), p_b2.clone()];
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![
        make_session("s1", vec![p_a1, p_b1]),
        make_session("s2", vec![p_a2, p_b2]),
    ];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
    assert_eq!(pairs.len(), 1);
    let state = pairs.values().next().unwrap();
    assert_eq!(state.sessions.len(), 2);
    let recur_count = state
        .evidence
        .iter()
        .filter(|e| e.technique == Technique::SessionRecurrence)
        .count();
    assert_eq!(recur_count, 1, "one extra session → one recurrence row");
}

#[test]
fn three_sessions_produce_two_recurrence_rows() {
    let mk = |sid: &str| vec![make_part("a.rs", 1, 10, sid, 0), make_part("b.rs", 1, 10, sid, 1)];
    let s1 = mk("s1");
    let s2 = mk("s2");
    let s3 = mk("s3");
    let all_parts: Vec<_> = [s1.clone(), s2.clone(), s3.clone()].concat();
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![
        make_session("s1", s1),
        make_session("s2", s2),
        make_session("s3", s3),
    ];
    let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
    let state = pairs.values().next().unwrap();
    let recur_count = state
        .evidence
        .iter()
        .filter(|e| e.technique == Technique::SessionRecurrence)
        .count();
    assert_eq!(recur_count, 2, "three sessions → two recurrence rows");
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn pair_evidence_is_deterministic_across_runs() {
    let mk_parts = || {
        vec![
            make_part("a.rs", 1, 20, "s1", 0),
            make_part("b.rs", 1, 20, "s1", 2),
            make_part("c.rs", 1, 10, "s2", 0),
            make_part("a.rs", 1, 20, "s2", 1),
        ]
    };
    let all_parts = mk_parts();
    let canonical = build_canonical_ranges(&all_parts, &cfg());
    let sessions = vec![
        make_session("s1", vec![make_part("a.rs", 1, 20, "s1", 0), make_part("b.rs", 1, 20, "s1", 2)]),
        make_session("s2", vec![make_part("c.rs", 1, 10, "s2", 0), make_part("a.rs", 1, 20, "s2", 1)]),
    ];

    let pairs1 = build_pair_evidence(&sessions, &canonical, &cfg());
    let pairs2 = build_pair_evidence(&sessions, &canonical, &cfg());
    let keys1: Vec<_> = pairs1.keys().copied().collect();
    let keys2: Vec<_> = pairs2.keys().copied().collect();
    assert_eq!(keys1, keys2, "keys must be in stable BTreeMap order");
}
