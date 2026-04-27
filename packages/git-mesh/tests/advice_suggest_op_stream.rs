//! Integration tests for the op-stream stage.
//!
//! Tests the public `build_op_stream` API using hand-built `SessionRecord`s.

use git_mesh::advice::session::state::{ReadRecord, TouchInterval};
use git_mesh::advice::suggest::{build_op_stream, OpKind, SessionRecord, SuggestConfig};

fn cfg() -> SuggestConfig {
    SuggestConfig::default()
}

fn read(path: &str, start: Option<u32>, end: Option<u32>, ts: &str) -> ReadRecord {
    ReadRecord { path: path.to_string(), start_line: start, end_line: end, ts: ts.to_string() }
}

fn touch(path: &str, start: u32, end: u32, ts: &str) -> TouchInterval {
    TouchInterval { path: path.to_string(), start_line: start, end_line: end, ts: ts.to_string() }
}

fn session(reads: Vec<ReadRecord>, touches: Vec<TouchInterval>) -> SessionRecord {
    SessionRecord { sid: "test".to_string(), reads, touches }
}

// ---------------------------------------------------------------------------
// dump-drop: tree-diff dumps are removed from the stream
// ---------------------------------------------------------------------------

#[test]
fn dump_op_is_dropped_from_stream() {
    // Three whole-file reads at the same timestamp → TREE_DIFF_BURST (3) → all dropped.
    let ts = "2024-01-01T00:00:00Z";
    let s = session(
        vec![
            read("a.rs", None, None, ts),
            read("b.rs", None, None, ts),
            read("c.rs", None, None, ts),
        ],
        vec![],
    );
    let ops = build_op_stream(&s, &cfg());
    assert!(ops.is_empty(), "dump reads (size >= TREE_DIFF_BURST) must be dropped; got {}", ops.len());
}

#[test]
fn drop_op_is_dropped_from_stream() {
    // Three whole-file edits at the same timestamp → dropped.
    let ts = "2024-01-01T00:00:00Z";
    let s = session(
        vec![],
        vec![touch("a.rs", 0, 0, ts), touch("b.rs", 0, 0, ts), touch("c.rs", 0, 0, ts)],
    );
    let ops = build_op_stream(&s, &cfg());
    assert!(ops.is_empty(), "dump edits (size >= TREE_DIFF_BURST) must be dropped; got {}", ops.len());
}

// ---------------------------------------------------------------------------
// edit-coalesce: consecutive edit ops on the same file are merged
// ---------------------------------------------------------------------------

#[test]
fn adjacent_edits_within_tolerance_are_coalesced() {
    // Two consecutive whole-file edits on the same path → coalesced into one op.
    let ts1 = "2024-01-01T00:00:01Z";
    let ts2 = "2024-01-01T00:00:02Z";
    let s = session(vec![], vec![touch("foo.rs", 0, 0, ts1), touch("foo.rs", 0, 0, ts2)]);
    let ops = build_op_stream(&s, &cfg());
    assert_eq!(ops.len(), 1, "two consecutive same-path edits must coalesce");
    assert_eq!(ops[0].count, 2, "coalesced op must carry count=2");
    assert_eq!(ops[0].kind, OpKind::Edit);
}

#[test]
fn edits_beyond_tolerance_remain_separate() {
    // Two whole-file edits on different paths → not coalesced.
    let ts1 = "2024-01-01T00:00:01Z";
    let ts2 = "2024-01-01T00:00:02Z";
    let s = session(vec![], vec![touch("a.rs", 0, 0, ts1), touch("b.rs", 0, 0, ts2)]);
    let ops = build_op_stream(&s, &cfg());
    assert_eq!(ops.len(), 2, "different-path edits must remain separate");
}

#[test]
fn edit_weight_bump_applied_to_coalesced_interval() {
    // Coalesced Edit ops carry a count > 1 which the downstream scoring uses
    // with `edit_weight_bump`.  Verify the count is preserved.
    let ts1 = "2024-01-01T00:00:01Z";
    let ts2 = "2024-01-01T00:00:02Z";
    let ts3 = "2024-01-01T00:00:03Z";
    let s = session(
        vec![],
        vec![touch("foo.rs", 0, 0, ts1), touch("foo.rs", 0, 0, ts2), touch("foo.rs", 0, 0, ts3)],
    );
    let ops = build_op_stream(&s, &cfg());
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].count, 3, "count must reflect all coalesced edit events");
}
