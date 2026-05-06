//! Phase F.2: single-session canonical range behavior.
//!
//! Participants come from a single session in production; per-session
//! precedence resolution (`resolve_extent_precedence`) already handles
//! the Whole-vs-narrower case before this stage. These tests verify that
//! `build_canonical_ranges` does NOT perform cross-session dedup — each
//! session's participants produce independent canonicals.

use git_mesh::advice::suggest::canonical::{build_canonical_ranges, part_key};
use git_mesh::advice::suggest::participants::{
    ExtentSource, Participant, ParticipantKind, WHOLE_FILE_END, WHOLE_FILE_START,
};
use git_mesh::advice::suggest::SuggestConfig;

fn ranged_edit(path: &str, start: u32, end: u32, op_index: usize) -> Participant {
    Participant {
        path: path.to_string(),
        start,
        end,
        op_index,
        kind: ParticipantKind::Edit,
        m_start: start,
        m_end: end,
        anchored: true,
        locator_distance: None,
        locator_forward: None,
        extent_source: ExtentSource::Edit,
    }
}

fn whole_read(path: &str, op_index: usize) -> Participant {
    Participant {
        path: path.to_string(),
        start: WHOLE_FILE_START,
        end: WHOLE_FILE_END,
        op_index,
        kind: ParticipantKind::Read,
        m_start: WHOLE_FILE_START,
        m_end: WHOLE_FILE_END,
        anchored: false,
        locator_distance: None,
        locator_forward: None,
        extent_source: ExtentSource::Whole,
    }
}

#[test]
fn each_session_produces_independent_canonicals() {
    // Two sessions each produce a participant on foo.rs — session A has a
    // ranged Edit (10-20), session B has a whole-file Read. Since
    // `build_canonical_ranges` does not perform cross-session dedup, both
    // canonicals survive independently.
    let session_a = ranged_edit("foo.rs", 10, 20, 0);
    let session_b = whole_read("foo.rs", 1);
    let parts = vec![session_a.clone(), session_b.clone()];

    let canonical = build_canonical_ranges(&parts, &SuggestConfig::default());

    // Both canonicals survive — two distinct canonicals for foo.rs.
    let foo_canonicals: Vec<_> = canonical
        .ranges
        .iter()
        .filter(|r| r.path == "foo.rs")
        .collect();
    assert_eq!(
        foo_canonicals.len(),
        2,
        "expected two independent canonicals for foo.rs (ranged + whole); got {:?}",
        canonical.ranges
    );

    // Each participant maps to its own distinct canonical — no cross-session
    // merging or dedup. The whole canonical (m_start=1) sorts before the
    // ranged (m_start=10) in component building, so it gets cid=0.
    let ranged_cid = canonical.canonical_id_of[&part_key(&session_a)];
    let whole_cid = canonical.canonical_id_of[&part_key(&session_b)];
    assert_eq!(whole_cid, 0);
    assert_eq!(ranged_cid, 1);
    assert_ne!(ranged_cid, whole_cid);
}

#[test]
fn whole_only_path_still_produces_canonical() {
    // bar.rs only ever appears as a whole-file read — its canonical must
    // survive (single path with no narrower sibling to suppress it).
    let parts = vec![
        ranged_edit("foo.rs", 10, 20, 0),
        whole_read("foo.rs", 1),
        whole_read("bar.rs", 2),
    ];
    let canonical = build_canonical_ranges(&parts, &SuggestConfig::default());

    let bar: Vec<_> = canonical.ranges.iter().filter(|r| r.path == "bar.rs").collect();
    assert_eq!(bar.len(), 1);
    assert_eq!(bar[0].source, ExtentSource::Whole);

    // canonical_id_of indices are dense within 0..ranges.len().
    for &id in canonical.canonical_id_of.values() {
        assert!(id < canonical.ranges.len());
    }
}
