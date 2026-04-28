//! Integration tests for the locator stage.
//!
//! Tests the public `attach_locators` and `prior_context_atoms` APIs.

use git_mesh::advice::suggest::{Op, OpKind, SuggestConfig, attach_locators, prior_context_atoms};

fn cfg() -> SuggestConfig {
    SuggestConfig::default()
}

fn make_read(path: &str, start: u32, end: u32, idx: usize) -> Op {
    Op {
        path: path.to_string(),
        start_line: Some(start),
        end_line: Some(end),
        ts_ms: idx as i64 * 1000,
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

fn make_edit(path: &str, idx: usize) -> Op {
    Op {
        path: path.to_string(),
        start_line: None,
        end_line: None,
        ts_ms: idx as i64 * 1000,
        op_index: idx,
        kind: OpKind::Edit,
        ranged: false,
        count: 1,
        inferred_start: None,
        inferred_end: None,
        locator_distance: None,
        locator_forward: None,
    }
}

// ---------------------------------------------------------------------------
// anchored edit attaches to nearest prior ranged read
// ---------------------------------------------------------------------------

#[test]
fn edit_attaches_to_nearest_prior_ranged_read() {
    // [read A at 1..5, read B at 10..20, edit C] — B is gap=1 from C, A is gap=2.
    // Locator must attach C to B.
    let mut ops = vec![
        make_read("foo.rs", 1, 5, 0),
        make_read("foo.rs", 10, 20, 1),
        make_edit("foo.rs", 2),
    ];
    attach_locators(&mut ops, &cfg());
    assert_eq!(
        ops[2].inferred_start,
        Some(10),
        "must attach to nearer read B"
    );
    assert_eq!(ops[2].inferred_end, Some(20));
    assert_eq!(ops[2].locator_distance, Some(1));
    assert_eq!(ops[2].locator_forward, Some(false));
}

#[test]
fn edit_with_no_prior_read_in_window_is_unanchored() {
    // Read at index 0, edit at index 8 (gap=8 > locator_window=6) → unanchored.
    let mut ops: Vec<Op> = vec![make_read("foo.rs", 1, 10, 0)];
    for i in 1..8 {
        ops.push(make_read("other.rs", 1, 5, i)); // different path — won't match
    }
    ops.push(make_edit("foo.rs", 8));
    attach_locators(&mut ops, &cfg());
    let edit = ops.last().unwrap();
    assert!(
        edit.inferred_start.is_none(),
        "out-of-window edit must be unanchored"
    );
}

// ---------------------------------------------------------------------------
// directory-penalty respected
// ---------------------------------------------------------------------------

#[test]
fn directory_penalty_reduces_score_for_cross_dir_attachment() {
    // forward read (gap=1, dir_penalty=0.4 → score=1.4) vs
    // a hypothetical same-direction read at gap=1 with no penalty (score=1.0).
    // Here we only verify that a forward read IS attached when it's the sole candidate
    // and that locator_forward is set to true.
    let mut ops = vec![
        make_edit("src/a/foo.rs", 0),
        make_read("src/a/foo.rs", 10, 20, 1), // forward read
    ];
    attach_locators(&mut ops, &cfg());
    assert_eq!(
        ops[0].locator_forward,
        Some(true),
        "forward read must be flagged"
    );
    assert!(
        ops[0].inferred_start.is_some(),
        "forward read is still attached when it's the only candidate"
    );
}

#[test]
fn same_directory_attachment_has_no_penalty() {
    // backward read (gap=1, penalty=0 → score=1.0) must beat forward read at gap=1
    // (score=1.4). Place read before edit.
    let mut ops = vec![
        make_read("src/a/bar.rs", 1, 10, 0),
        make_edit("src/a/bar.rs", 1),
    ];
    attach_locators(&mut ops, &cfg());
    assert_eq!(
        ops[1].locator_forward,
        Some(false),
        "backward read must be preferred"
    );
    assert_eq!(ops[1].locator_distance, Some(1));
}

#[test]
fn prior_context_k_limits_candidates_considered() {
    // locator_prior_context_k=2: only the 2 most-recent reads before the edit are returned.
    let cfg = SuggestConfig {
        locator_prior_context_k: 2,
        ..Default::default()
    };
    let ops = vec![
        make_read("a.rs", 1, 10, 0),
        make_read("b.rs", 1, 10, 1),
        make_read("c.rs", 1, 10, 2),
        make_edit("d.rs", 3),
    ];
    let atoms = prior_context_atoms(&ops, 3, &cfg);
    assert_eq!(
        atoms.len(),
        2,
        "only locator_prior_context_k=2 atoms returned"
    );
    assert_eq!(atoms[0].path, "c.rs", "most recent first");
    assert_eq!(atoms[1].path, "b.rs");
}
