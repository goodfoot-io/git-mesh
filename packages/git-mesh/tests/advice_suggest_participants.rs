//! Integration tests for the participants + anchor-merge stage.

use git_mesh::advice::suggest::{
    Op, OpKind, SuggestConfig, build_participants, merge_ranges_per_file,
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

// ---------------------------------------------------------------------------
// per-file anchor merge tolerance
// ---------------------------------------------------------------------------

#[test]
fn overlapping_intervals_merged_into_one_participant() {
    // [1,20] and [15,35] → merged to [1,35].
    let ops = vec![
        make_read_op("foo.rs", 1, 20, 0),
        make_read_op("foo.rs", 15, 35, 1),
    ];
    let parts = build_participants(&ops, "s1");
    let merged = merge_ranges_per_file(&parts, &cfg());
    assert!(merged.iter().all(|p| p.m_start == 1 && p.m_end == 35));
}

#[test]
fn intervals_within_tolerance_merged_into_one_participant() {
    // [1,10] and [15,25]: gap = 15-10-1 = 4 ≤ tolerance 5 → merged to [1,25].
    let ops = vec![
        make_read_op("foo.rs", 1, 10, 0),
        make_read_op("foo.rs", 15, 25, 1),
    ];
    let parts = build_participants(&ops, "s1");
    let merged = merge_ranges_per_file(&parts, &cfg());
    assert!(merged.iter().all(|p| p.m_start == 1 && p.m_end == 25));
}

#[test]
fn intervals_beyond_tolerance_remain_separate_participants() {
    // [1,10] and [20,30]: gap = 20-10-1 = 9 > tolerance 5 → stays separate.
    let ops = vec![
        make_read_op("foo.rs", 1, 10, 0),
        make_read_op("foo.rs", 20, 30, 1),
    ];
    let parts = build_participants(&ops, "s1");
    let merged = merge_ranges_per_file(&parts, &cfg());
    let p0 = merged.iter().find(|p| p.start == 1).unwrap();
    let p1 = merged.iter().find(|p| p.start == 20).unwrap();
    assert_eq!(p0.m_start, 1);
    assert_eq!(p0.m_end, 10);
    assert_eq!(p1.m_start, 20);
    assert_eq!(p1.m_end, 30);
}

// ---------------------------------------------------------------------------
// IoU edge cases (via range_iou in canonical, but exercised through participants)
// ---------------------------------------------------------------------------

#[test]
fn iou_of_identical_intervals_is_one() {
    use git_mesh::advice::suggest::build_participants;
    use git_mesh::advice::suggest::range_iou;
    let ops = vec![make_read_op("a.rs", 10, 30, 0)];
    let p1 = &build_participants(&ops, "s1")[0];
    let p2 = &build_participants(&ops, "s2")[0];
    let iou = range_iou(p1, p2);
    assert!(
        (iou - 1.0).abs() < 1e-9,
        "identical intervals → iou=1.0, got {iou}"
    );
}

#[test]
fn iou_of_non_overlapping_intervals_is_zero() {
    use git_mesh::advice::suggest::range_iou;
    let ops1 = vec![make_read_op("a.rs", 1, 10, 0)];
    let ops2 = vec![make_read_op("a.rs", 20, 30, 0)];
    let p1 = &build_participants(&ops1, "s1")[0];
    let p2 = &build_participants(&ops2, "s2")[0];
    let iou = range_iou(p1, p2);
    assert_eq!(iou, 0.0, "non-overlapping intervals → iou=0.0, got {iou}");
}

#[test]
fn iou_partial_overlap_is_computed_correctly() {
    use git_mesh::advice::suggest::range_iou;
    // [1,20] and [10,30]: inter=10..20=11, a_len=20, b_len=21, union=30 → 11/30
    let ops1 = vec![make_read_op("a.rs", 1, 20, 0)];
    let ops2 = vec![make_read_op("a.rs", 10, 30, 0)];
    let p1 = &build_participants(&ops1, "s1")[0];
    let p2 = &build_participants(&ops2, "s2")[0];
    let iou = range_iou(p1, p2);
    let expected = 11.0_f64 / 30.0;
    assert!(
        (iou - expected).abs() < 1e-9,
        "expected {expected}, got {iou}"
    );
}

#[test]
fn iou_below_threshold_does_not_create_edge() {
    use git_mesh::advice::suggest::{build_canonical_ranges, range_iou};
    // [1,10] and [20,30]: iou=0.0 < 0.30 → separate canonical ids.
    let ops1 = vec![make_read_op("a.rs", 1, 10, 0)];
    let ops2 = vec![make_read_op("a.rs", 20, 30, 0)];
    let mut all_parts = build_participants(&ops1, "s1");
    all_parts.extend(build_participants(&ops2, "s2"));
    let all_merged = merge_ranges_per_file(&all_parts, &cfg());
    let iou = range_iou(&all_merged[0], &all_merged[1]);
    assert!(
        iou < cfg().range_overlap_iou,
        "iou {iou} must be below threshold"
    );
    let idx = build_canonical_ranges(&all_merged, &cfg());
    assert_eq!(
        idx.ranges.len(),
        2,
        "non-overlapping ranges must produce two canonical ids"
    );
}
