//! Participants stage (Section 5 of analyze-v4.mjs).
//!
//! Turns the op-stream into a flat list of (path, anchor) atoms, then merges
//! near-touching ranges per file.

use std::collections::BTreeMap;

use crate::advice::suggest::op_stream::{Op, OpKind};
use crate::advice::suggest::SuggestConfig;

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
    /// Session id for canonical stage key.
    pub session_sid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParticipantKind {
    Read,
    TouchRead,
    Edit,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build a flat list of participants from the op-stream.
///
/// Ports `participants` from `docs/analyze-v4.mjs` line 258.
///
/// Only ranged ops contribute.  Whole-file edit ops without locator anchor
/// are dropped.  The `session_sid` is set from the caller.
pub fn participants(ops: &[Op], session_sid: &str) -> Vec<Participant> {
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
                    session_sid: session_sid.to_string(),
                });
            }
            OpKind::Edit => {
                if let (Some(inf_s), Some(inf_e)) = (op.inferred_start, op.inferred_end) {
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
                        session_sid: session_sid.to_string(),
                    });
                }
                // Unanchored edits are dropped (no inferred anchor).
            }
            _ => {}
        }
    }
    out
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
            Participant { m_start, m_end, ..p.clone() }
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
        let parts = participants(&ops, "s1");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].start, 1);
        assert_eq!(parts[0].end, 10);
    }

    #[test]
    fn unanchored_edits_excluded() {
        let ops = vec![make_edit_op("foo.rs", None, None, 0)];
        let parts = participants(&ops, "s1");
        assert!(parts.is_empty());
    }

    #[test]
    fn anchored_edits_included() {
        let ops = vec![make_edit_op("foo.rs", Some(5), Some(15), 0)];
        let parts = participants(&ops, "s1");
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
        let parts = participants(&ops, "s1");
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
        let parts = participants(&ops, "s1");
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
        let parts = participants(&ops, "s1");
        let merged = merge_ranges_per_file(&parts, &cfg());
        assert_eq!(merged[0].m_start, 1);
        assert_eq!(merged[0].m_end, 10);
        assert_eq!(merged[1].m_start, 20);
        assert_eq!(merged[1].m_end, 30);
    }

    #[test]
    fn different_files_not_merged() {
        let ops = vec![
            make_read_op("a.rs", 1, 10, 0),
            make_read_op("b.rs", 5, 15, 1),
        ];
        let parts = participants(&ops, "s1");
        let merged = merge_ranges_per_file(&parts, &cfg());
        let a = merged.iter().find(|p| p.path == "a.rs").unwrap();
        let b = merged.iter().find(|p| p.path == "b.rs").unwrap();
        assert_eq!(a.m_start, 1);
        assert_eq!(b.m_start, 5);
    }
}
