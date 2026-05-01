//! Integration tests for the canonical-id stage.

use git_mesh::advice::suggest::{
    Op, OpKind, SuggestConfig, build_canonical_ranges, build_participants, merge_ranges_per_file,
    range_iou,
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

fn parts_for(
    path: &str,
    start: u32,
    end: u32,
    _sid: &str,
) -> git_mesh::advice::suggest::Participant {
    let ops = vec![make_read_op(path, start, end, 0)];
    build_participants(&ops).into_iter().next().unwrap()
}

// ---------------------------------------------------------------------------
// IoU connected-components yields stable canonical ids per run
// ---------------------------------------------------------------------------

#[test]
fn same_input_produces_same_canonical_ids() {
    let parts = vec![
        parts_for("a.rs", 1, 20, "s1"),
        parts_for("a.rs", 10, 30, "s2"),
        parts_for("b.rs", 1, 10, "s3"),
    ];
    let idx1 = build_canonical_ranges(&parts, &cfg());
    let idx2 = build_canonical_ranges(&parts, &cfg());
    assert_eq!(
        idx1.ranges, idx2.ranges,
        "ranges must be identical across runs"
    );
    assert_eq!(
        idx1.canonical_id_of, idx2.canonical_id_of,
        "ids must be identical across runs"
    );
}

#[test]
fn disjoint_files_get_distinct_canonical_ids() {
    // [1,10] and [20,30] on the same path have no IoU edge → different canonical ids.
    let p0 = parts_for("a.rs", 1, 10, "s1");
    let p1 = parts_for("a.rs", 20, 30, "s2");
    let all = vec![p0.clone(), p1.clone()];
    let all_merged = merge_ranges_per_file(&all, &cfg());

    let iou = range_iou(&all_merged[0], &all_merged[1]);
    assert!(iou < cfg().range_overlap_iou);

    let idx = build_canonical_ranges(&all_merged, &cfg());
    let id0 = idx.canonical_id_of[&git_mesh::advice::suggest::canonical::part_key(&all_merged[0])];
    let id1 = idx.canonical_id_of[&git_mesh::advice::suggest::canonical::part_key(&all_merged[1])];
    assert_ne!(id0, id1, "disjoint ranges must get distinct canonical ids");
}

#[test]
fn connected_files_get_same_canonical_id() {
    // [1,20] and [10,30]: iou=11/30≈0.37 ≥ 0.30 → same component.
    let p0 = parts_for("a.rs", 1, 20, "s1");
    let p1 = parts_for("a.rs", 10, 30, "s2");
    let all = vec![p0.clone(), p1.clone()];
    let idx = build_canonical_ranges(&all, &cfg());
    let id0 = idx.canonical_id_of[&git_mesh::advice::suggest::canonical::part_key(&all[0])];
    let id1 = idx.canonical_id_of[&git_mesh::advice::suggest::canonical::part_key(&all[1])];
    assert_eq!(id0, id1, "overlapping ranges must share a canonical id");
}

#[test]
fn adding_unrelated_file_does_not_change_existing_ids() {
    let p0 = parts_for("a.rs", 1, 20, "s1");
    let p1 = parts_for("a.rs", 10, 30, "s2");
    let base = vec![p0.clone(), p1.clone()];
    let idx_base = build_canonical_ranges(&base, &cfg());

    let mut extended = base.clone();
    extended.push(parts_for("b.rs", 100, 200, "s3"));
    let idx_ext = build_canonical_ranges(&extended, &cfg());

    for p in &base {
        let k = git_mesh::advice::suggest::canonical::part_key(p);
        assert_eq!(
            idx_base.canonical_id_of[&k], idx_ext.canonical_id_of[&k],
            "adding unrelated file must not change existing ids"
        );
    }
}

#[test]
fn transitive_edges_expand_component() {
    // A=[1,20], B=[10,30], C=[20,40]:
    //   iou(A,B) ≈ 0.37 ≥ 0.30, iou(B,C) ≈ 0.35 ≥ 0.30, iou(A,C) ≈ 0.025 < 0.30.
    // All three must end up in the same component via B.
    let p0 = parts_for("a.rs", 1, 20, "s1");
    let p1 = parts_for("a.rs", 10, 30, "s2");
    let p2 = parts_for("a.rs", 20, 40, "s3");
    let all = vec![p0.clone(), p1.clone(), p2.clone()];
    let idx = build_canonical_ranges(&all, &cfg());
    let ids: Vec<usize> = all
        .iter()
        .map(|p| idx.canonical_id_of[&git_mesh::advice::suggest::canonical::part_key(p)])
        .collect();
    assert_eq!(ids[0], ids[1], "A and B must share canonical id");
    assert_eq!(ids[1], ids[2], "B and C must share canonical id");
}
