//! Participants stage (Section 5 of analyze-v4.mjs).
//!
//! Turns the op-stream into a flat list of (path, anchor) atoms, then merges
//! near-touching ranges per file.

use std::collections::BTreeMap;

use crate::advice::suggest::SuggestConfig;
use crate::advice::suggest::op_stream::{Op, OpKind};

// ── Public types ──────────────────────────────────────────────────────────────

/// A (path, anchor) atom with provenance from one op.
///
/// After `merge_ranges_per_file`, `m_start`/`m_end` hold the merged anchor
/// that this participant was absorbed into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub path: String,
    /// Original start from the op.
    pub start: u32,
    /// Original end from the op.
    pub end: u32,
    /// Sequential index in the op-stream.
    pub op_index: usize,
    pub kind: ParticipantKind,
    /// Merged start (set by `merge_ranges_per_file`).
    pub m_start: u32,
    /// Merged end (set by `merge_ranges_per_file`).
    pub m_end: u32,
    // Edit-specific fields (None for Read/TouchRead).
    pub anchored: bool,
    pub locator_distance: Option<u32>,
    pub locator_forward: Option<bool>,
    /// Which evidence branch produced this participant's extent.
    pub extent_source: ExtentSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParticipantKind {
    Read,
    TouchRead,
    Edit,
}

/// Origin of a participant's line-range extent. Resolved by precedence
/// `Symbol > Edit > Read > Whole` per `(session, path)` in
/// `resolve_extent_precedence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtentSource {
    Symbol,
    Edit,
    Read,
    Whole,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Sentinel start/end for whole-file participants: a participant with
/// `m_start == WHOLE_FILE_START && m_end == WHOLE_FILE_END` represents a
/// whole-file touch or read with no specific line anchor.
pub const WHOLE_FILE_START: u32 = 1;
pub const WHOLE_FILE_END: u32 = u32::MAX;

/// Build a flat list of participants from the op-stream.
///
/// Ports `participants` from `docs/analyze-v4.mjs` line 258.
///
/// Ranged reads/edits contribute their anchor; whole-file reads and unanchored
/// whole-file edits emit a sentinel whole-file participant
/// (`m_start = WHOLE_FILE_START`, `m_end = WHOLE_FILE_END`) so a Modified-only
/// turn still surfaces participants — the suggester downstream can then walk
/// history and produce co-change suggestions even without ranged anchors.
pub fn participants(ops: &[Op]) -> Vec<Participant> {
    let mut out = Vec::new();
    for op in ops {
        match op.kind {
            OpKind::Read | OpKind::TouchRead if op.ranged => {
                let (start, end) = match (op.start_line, op.end_line) {
                    (Some(s), Some(e)) => (s, e),
                    _ => continue,
                };
                let pk = if op.kind == OpKind::Read {
                    ParticipantKind::Read
                } else {
                    ParticipantKind::TouchRead
                };
                out.push(Participant {
                    path: op.path.clone(),
                    start,
                    end,
                    op_index: op.op_index,
                    kind: pk,
                    m_start: start,
                    m_end: end,
                    anchored: false,
                    locator_distance: None,
                    locator_forward: None,
                    extent_source: ExtentSource::Read,
                });
            }
            OpKind::Read | OpKind::TouchRead => {
                // Whole-file read (no line range) — emit a sentinel whole-file
                // participant so co-change history can still surface a pair.
                let pk = if op.kind == OpKind::Read {
                    ParticipantKind::Read
                } else {
                    ParticipantKind::TouchRead
                };
                out.push(Participant {
                    path: op.path.clone(),
                    start: WHOLE_FILE_START,
                    end: WHOLE_FILE_END,
                    op_index: op.op_index,
                    kind: pk,
                    m_start: WHOLE_FILE_START,
                    m_end: WHOLE_FILE_END,
                    anchored: false,
                    locator_distance: None,
                    locator_forward: None,
                    extent_source: ExtentSource::Whole,
                });
            }
            OpKind::Edit => {
                if op.ranged
                    && let (Some(s), Some(e)) = (op.start_line, op.end_line)
                {
                    out.push(Participant {
                        path: op.path.clone(),
                        start: s,
                        end: e,
                        op_index: op.op_index,
                        kind: ParticipantKind::Edit,
                        m_start: s,
                        m_end: e,
                        anchored: true,
                        locator_distance: op.locator_distance,
                        locator_forward: op.locator_forward,
                        extent_source: ExtentSource::Edit,
                    });
                } else if let (Some(inf_s), Some(inf_e)) = (op.inferred_start, op.inferred_end) {
                    out.push(Participant {
                        path: op.path.clone(),
                        start: inf_s,
                        end: inf_e,
                        op_index: op.op_index,
                        kind: ParticipantKind::Edit,
                        m_start: inf_s,
                        m_end: inf_e,
                        anchored: true,
                        locator_distance: op.locator_distance,
                        locator_forward: op.locator_forward,
                        extent_source: ExtentSource::Edit,
                    });
                } else {
                    // Whole-file Modified touch with no inferable anchor.
                    // Emit a sentinel whole-file Edit participant so the file
                    // appears as a participant in this turn.
                    out.push(Participant {
                        path: op.path.clone(),
                        start: WHOLE_FILE_START,
                        end: WHOLE_FILE_END,
                        op_index: op.op_index,
                        kind: ParticipantKind::Edit,
                        m_start: WHOLE_FILE_START,
                        m_end: WHOLE_FILE_END,
                        anchored: false,
                        locator_distance: None,
                        locator_forward: None,
                        extent_source: ExtentSource::Whole,
                    });
                }
            }
        }
    }
    out
}

/// Apply precedence: Symbol > Edit > Read > Whole per (session, path).
///
/// Phase A delivers the signature only; Phase C implements the per-path
/// resolution that drops whole-file siblings when a narrower source exists.
/// Until then this is a passthrough so the suggester pipeline behaves
/// identically to before the `ExtentSource` plumbing landed.
pub fn resolve_extent_precedence(parts: Vec<Participant>) -> Vec<Participant> {
    parts
}

/// Merge near-touching ranges of the same file within a single session.
///
/// Ports `mergeRangesPerFile` from `docs/analyze-v4.mjs` line 275.
///
/// Sets `m_start`/`m_end` on each participant to the merged interval that
/// contains it.  The returned vec has the same length and order as the input.
pub fn merge_ranges_per_file(parts: &[Participant], cfg: &SuggestConfig) -> Vec<Participant> {
    let tolerance = cfg.range_merge_tolerance as i64;

    // Build merged groups per path using BTreeMap for deterministic iteration.
    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in parts.iter().enumerate() {
        by_file.entry(p.path.as_str()).or_default().push(i);
    }

    // For each file, sort indices by start, build merged groups.
    // Each merged group is (m_start, m_end).
    // Then map back to participant indices.
    let mut merged_ranges: Vec<(u32, u32)> = vec![(0, 0); parts.len()];

    for idxs in by_file.values() {
        // Sort by start line.
        let mut sorted = idxs.clone();
        sorted.sort_by_key(|&i| parts[i].start);

        // Build merged groups: vec of (m_start, m_end, member_indices).
        let mut groups: Vec<(u32, u32, Vec<usize>)> = Vec::new();
        for &i in &sorted {
            let p = &parts[i];
            if let Some(last) = groups.last_mut()
                && p.start as i64 <= last.1 as i64 + tolerance
            {
                last.1 = last.1.max(p.end);
                last.2.push(i);
                continue;
            }
            groups.push((p.start, p.end, vec![i]));
        }

        // Assign merged ranges back.
        for (m_start, m_end, members) in groups {
            for i in members {
                merged_ranges[i] = (m_start, m_end);
            }
        }
    }

    // Rebuild participants with updated m_start/m_end.
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (m_start, m_end) = merged_ranges[i];
            Participant {
                m_start,
                m_end,
                ..p.clone()
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::op_stream::{Op, OpKind};

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

    fn make_edit_op(path: &str, inf_s: Option<u32>, inf_e: Option<u32>, idx: usize) -> Op {
        Op {
            path: path.to_string(),
            start_line: None,
            end_line: None,
            ts_ms: idx as i64,
            op_index: idx,
            kind: OpKind::Edit,
            ranged: false,
            count: 1,
            inferred_start: inf_s,
            inferred_end: inf_e,
            locator_distance: None,
            locator_forward: None,
        }
    }

    #[test]
    fn ranged_reads_become_participants() {
        let ops = vec![make_read_op("foo.rs", 1, 10, 0)];
        let parts = participants(&ops);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].start, 1);
        assert_eq!(parts[0].end, 10);
    }

    #[test]
    fn unanchored_edits_become_whole_file_participants() {
        // A whole-file Modified touch with no inferable anchor surfaces as a
        // sentinel whole-file Edit participant so its file still shows up as
        // a co-change candidate downstream.
        let ops = vec![make_edit_op("foo.rs", None, None, 0)];
        let parts = participants(&ops);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, ParticipantKind::Edit);
        assert_eq!(parts[0].m_start, WHOLE_FILE_START);
        assert_eq!(parts[0].m_end, WHOLE_FILE_END);
    }

    fn make_ranged_edit_op(path: &str, start: u32, end: u32, idx: usize) -> Op {
        Op {
            path: path.to_string(),
            start_line: Some(start),
            end_line: Some(end),
            ts_ms: idx as i64,
            op_index: idx,
            kind: OpKind::Edit,
            ranged: true,
            count: 1,
            inferred_start: None,
            inferred_end: None,
            locator_distance: None,
            locator_forward: None,
        }
    }

    #[test]
    fn ranged_edit_op_becomes_ranged_edit_participant() {
        // Phase B.2: an Edit op carrying op.ranged + start/end produces a
        // Participant with extent_source=Edit and the hunk's exact range.
        let ops = vec![make_ranged_edit_op("foo.rs", 88, 120, 0)];
        let parts = participants(&ops);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, ParticipantKind::Edit);
        assert_eq!(parts[0].start, 88);
        assert_eq!(parts[0].end, 120);
        assert_eq!(parts[0].m_start, 88);
        assert_eq!(parts[0].m_end, 120);
        assert_eq!(parts[0].extent_source, ExtentSource::Edit);
        assert!(parts[0].anchored);
    }

    #[test]
    fn ranged_touch_drives_ranged_edit_participant_end_to_end() {
        // Phase B Spike 1: drive a ranged TouchInterval through
        // build_op_stream → participants and assert the resulting participant
        // carries the hunk extent with extent_source=Edit.
        use crate::advice::session::state::{TouchInterval, TouchKind};
        use crate::advice::suggest::op_stream::{SessionRecord, build_op_stream};

        let touch = TouchInterval {
            path: "foo.rs".to_string(),
            kind: TouchKind::Modified,
            id: "tuid-1".to_string(),
            ts: "2024-01-01T00:00:01Z".to_string(),
            start: Some(10),
            end: Some(20),
        };
        let session = SessionRecord {
            sid: "s".to_string(),
            reads: vec![],
            touches: vec![touch],
        };
        let ops = build_op_stream(&session, &cfg());
        let parts = participants(&ops);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "foo.rs");
        assert_eq!(parts[0].start, 10);
        assert_eq!(parts[0].end, 20);
        assert_eq!(parts[0].kind, ParticipantKind::Edit);
        assert_eq!(parts[0].extent_source, ExtentSource::Edit);
    }

    #[test]
    fn anchored_edits_included() {
        let ops = vec![make_edit_op("foo.rs", Some(5), Some(15), 0)];
        let parts = participants(&ops);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, ParticipantKind::Edit);
        assert_eq!(parts[0].start, 5);
        assert_eq!(parts[0].end, 15);
    }

    #[test]
    fn overlapping_intervals_merged() {
        // [1,20] and [15,35] → merged to [1,35]
        let ops = vec![
            make_read_op("foo.rs", 1, 20, 0),
            make_read_op("foo.rs", 15, 35, 1),
        ];
        let parts = participants(&ops);
        let merged = merge_ranges_per_file(&parts, &cfg());
        assert!(merged.iter().all(|p| p.m_start == 1 && p.m_end == 35));
    }

    #[test]
    fn intervals_within_tolerance_merged() {
        // [1,10] and [15,25], gap = 4 <= tolerance 5 → merged
        let ops = vec![
            make_read_op("foo.rs", 1, 10, 0),
            make_read_op("foo.rs", 15, 25, 1),
        ];
        let parts = participants(&ops);
        let merged = merge_ranges_per_file(&parts, &cfg());
        assert!(merged.iter().all(|p| p.m_start == 1 && p.m_end == 25));
    }

    #[test]
    fn intervals_beyond_tolerance_stay_separate() {
        // [1,10] and [20,30], gap = 9 > tolerance 5 → separate
        let ops = vec![
            make_read_op("foo.rs", 1, 10, 0),
            make_read_op("foo.rs", 20, 30, 1),
        ];
        let parts = participants(&ops);
        let merged = merge_ranges_per_file(&parts, &cfg());
        assert_eq!(merged[0].m_start, 1);
        assert_eq!(merged[0].m_end, 10);
        assert_eq!(merged[1].m_start, 20);
        assert_eq!(merged[1].m_end, 30);
    }

    fn make_whole_read_op(path: &str, idx: usize) -> Op {
        Op {
            path: path.to_string(),
            start_line: None,
            end_line: None,
            ts_ms: idx as i64,
            op_index: idx,
            kind: OpKind::Read,
            ranged: false,
            count: 1,
            inferred_start: None,
            inferred_end: None,
            locator_distance: None,
            locator_forward: None,
        }
    }

    #[test]
    fn extent_source_assigned_correctly_per_branch() {
        // Phase A delivers: each construction branch tags the right source.
        let ops = vec![
            make_read_op("ranged_read.rs", 1, 10, 0),
            make_whole_read_op("whole_read.rs", 1),
            make_edit_op("ranged_edit.rs", Some(5), Some(15), 2),
            make_edit_op("whole_edit.rs", None, None, 3),
        ];
        let parts = participants(&ops);
        let by_path = |path: &str| {
            parts
                .iter()
                .find(|p| p.path == path)
                .expect("participant for path")
                .extent_source
        };
        assert_eq!(by_path("ranged_read.rs"), ExtentSource::Read);
        assert_eq!(by_path("whole_read.rs"), ExtentSource::Whole);
        assert_eq!(by_path("ranged_edit.rs"), ExtentSource::Edit);
        assert_eq!(by_path("whole_edit.rs"), ExtentSource::Whole);
    }

    #[test]
    #[ignore = "Phase C: precedence resolution"]
    fn whole_file_read_dropped_when_ranged_read_present_for_same_path() {
        let ops = vec![
            make_read_op("foo.rs", 1, 80, 0),
            make_whole_read_op("foo.rs", 1),
        ];
        let parts = participants(&ops);
        let resolved = resolve_extent_precedence(parts);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].extent_source, ExtentSource::Read);
        assert_eq!(resolved[0].m_start, 1);
        assert_eq!(resolved[0].m_end, 80);
    }

    #[test]
    #[ignore = "Phase C: precedence resolution (depends on Phase B ranged Edit ops)"]
    fn whole_file_edit_dropped_when_ranged_edit_present_for_same_path() {
        let ops = vec![
            make_edit_op("foo.rs", Some(88), Some(120), 0),
            make_edit_op("foo.rs", None, None, 1),
        ];
        let parts = participants(&ops);
        let resolved = resolve_extent_precedence(parts);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].extent_source, ExtentSource::Edit);
        assert_eq!(resolved[0].m_start, 88);
        assert_eq!(resolved[0].m_end, 120);
    }

    #[test]
    #[ignore = "Phase C: precedence resolution (Edit beats Read on same path)"]
    fn ranged_read_dropped_when_ranged_edit_present_for_same_path() {
        let ops = vec![
            make_read_op("foo.rs", 1, 50, 0),
            make_edit_op("foo.rs", Some(20), Some(30), 1),
        ];
        let parts = participants(&ops);
        let resolved = resolve_extent_precedence(parts);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].extent_source, ExtentSource::Edit);
    }

    #[test]
    #[ignore = "Phase C: precedence resolution (whole-only path keeps sentinel)"]
    fn whole_file_only_path_keeps_whole_sentinel() {
        let ops = vec![make_whole_read_op("foo.rs", 0)];
        let parts = participants(&ops);
        let resolved = resolve_extent_precedence(parts);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].extent_source, ExtentSource::Whole);
        assert_eq!(resolved[0].m_start, WHOLE_FILE_START);
        assert_eq!(resolved[0].m_end, WHOLE_FILE_END);
    }

    #[test]
    #[ignore = "Phase E: empty-hull defense + extent-drop debug line"]
    fn empty_hull_falls_back_to_whole_with_debug_line() {
        // TODO Phase E: construct corrupt participants whose convex hull
        // computes start > end, run through canonical-range step, and assert
        // the result is whole-file with a `git-mesh-advice-debug` line of
        // event=extent-drop, reason=empty-hull.
    }

    #[test]
    #[ignore = "Phase B/C: end-to-end suggester carries narrow ranges through"]
    fn end_to_end_suggester_emits_per_path_extents() {
        // TODO Phase B/C: drive the suggester with one whole-file Read of
        // compact.rs, one ranged Read of stale_output.rs#L1-L80, one whole-file
        // Read of mod.rs, plus a structuredPatch Edit on compact.rs#L88-L120,
        // and assert the emitted Suggestion participants are
        //   - compact.rs#L88-L120 (extent_source=Edit)
        //   - stale_output.rs#L1-L80 (extent_source=Read)
        //   - mod.rs (whole, extent_source=Whole)
    }

    #[test]
    fn different_files_not_merged() {
        let ops = vec![
            make_read_op("a.rs", 1, 10, 0),
            make_read_op("b.rs", 5, 15, 1),
        ];
        let parts = participants(&ops);
        let merged = merge_ranges_per_file(&parts, &cfg());
        let a = merged.iter().find(|p| p.path == "a.rs").unwrap();
        let b = merged.iter().find(|p| p.path == "b.rs").unwrap();
        assert_eq!(a.m_start, 1);
        assert_eq!(b.m_start, 5);
    }
}
