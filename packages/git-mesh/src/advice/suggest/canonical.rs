//! Canonical ranges stage (Section 6 of analyze-v4.mjs).
//!
//! Two ranges across sessions match when same path AND IoU >= threshold.
//! Connected components under this relation become canonical ranges.

use std::collections::BTreeMap;

use crate::advice::suggest::participants::Participant;
use crate::advice::suggest::SuggestConfig;

// ── Public types ──────────────────────────────────────────────────────────────

/// A canonical anchor: the bounding box of a connected component of
/// cross-session participants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalRange {
    pub path: String,
    pub start: u32,
    pub end: u32,
}

/// Maps each participant's key to a canonical anchor id (index into
/// `CanonicalIndex::ranges`).
///
/// The key is `partKey(p)` = `"{path}#{m_start}-{m_end}#{session_sid}#{op_index}"`.
pub struct CanonicalIndex {
    pub ranges: Vec<CanonicalRange>,
    /// Map from `part_key(p)` → index into `ranges`.
    pub canonical_id_of: BTreeMap<String, usize>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Stable key for a participant, mirroring JS `partKey`.
pub fn part_key(p: &Participant) -> String {
    format!("{}#{}-{}#{}#{}", p.path, p.m_start, p.m_end, p.session_sid, p.op_index)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compute the intersection-over-union of two participant ranges.
///
/// Returns 0.0 when paths differ or ranges don't overlap.
///
/// Ports `rangeIoU` from `docs/analyze-v4.mjs` line 304.
/// Uses `m_start`/`m_end` (post-merge ranges).
pub fn range_iou(a: &Participant, b: &Participant) -> f64 {
    if a.path != b.path {
        return 0.0;
    }
    let lo = a.m_start.max(b.m_start);
    let hi = a.m_end.min(b.m_end);
    if hi < lo {
        return 0.0;
    }
    let inter = (hi - lo + 1) as f64;
    let a_len = (a.m_end - a.m_start + 1) as f64;
    let b_len = (b.m_end - b.m_start + 1) as f64;
    inter / (a_len + b_len - inter)
}

/// Build the canonical anchor index from all (merged) participants across all
/// sessions.
///
/// Ports `buildCanonicalRanges` from `docs/analyze-v4.mjs` line 313.
///
/// Uses `BTreeMap` and sorted iteration to guarantee determinism across runs.
pub fn build_canonical_ranges(all_parts: &[Participant], cfg: &SuggestConfig) -> CanonicalIndex {
    let threshold = cfg.range_overlap_iou;

    // Group participants by path using BTreeMap (sorted iteration).
    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in all_parts.iter().enumerate() {
        by_file.entry(p.path.as_str()).or_default().push(i);
    }

    let mut canonical_ranges: Vec<CanonicalRange> = Vec::new();
    let mut canonical_id_of: BTreeMap<String, usize> = BTreeMap::new();

    for (path, idxs) in &by_file {
        // Sort by m_start for deterministic component building.
        let mut sorted = idxs.clone();
        sorted.sort_by_key(|&i| (all_parts[i].m_start, all_parts[i].m_end));

        let n = sorted.len();
        let mut assigned: Vec<Option<usize>> = vec![None; n];

        // Union-Find / greedy connected-components (mirrors JS algorithm).
        let mut components: Vec<Vec<usize>> = Vec::new();

        for i in 0..n {
            if assigned[i].is_some() {
                continue;
            }
            let comp_id = components.len();
            let mut comp: Vec<usize> = vec![i];
            assigned[i] = Some(comp_id);

            for j in (i + 1)..n {
                if assigned[j].is_some() {
                    continue;
                }
                // Check if j is connected to any already-in-comp member.
                let in_comp = comp.iter().any(|&k| {
                    let pk = all_parts[sorted[k]].clone();
                    let pj = all_parts[sorted[j]].clone();
                    range_iou(&pk, &pj) >= threshold
                });
                if in_comp {
                    comp.push(j);
                    assigned[j] = Some(comp_id);
                }
            }
            components.push(comp);
        }

        // For each component, compute bounding box and assign canonical id.
        for comp in &components {
            let lo = comp.iter().map(|&k| all_parts[sorted[k]].m_start).min().unwrap();
            let hi = comp.iter().map(|&k| all_parts[sorted[k]].m_end).max().unwrap();
            let cid = canonical_ranges.len();
            canonical_ranges.push(CanonicalRange {
                path: path.to_string(),
                start: lo,
                end: hi,
            });
            for &k in comp {
                let key = part_key(&all_parts[sorted[k]]);
                canonical_id_of.insert(key, cid);
            }
        }
    }

    CanonicalIndex { ranges: canonical_ranges, canonical_id_of }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::participants::{Participant, ParticipantKind};

    fn cfg() -> SuggestConfig {
        SuggestConfig::default()
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

    #[test]
    fn iou_identical_ranges() {
        let a = make_part("a.rs", 10, 30, "s1", 0);
        let b = make_part("a.rs", 10, 30, "s2", 0);
        let iou = range_iou(&a, &b);
        assert!((iou - 1.0).abs() < 1e-9);
    }

    #[test]
    fn iou_non_overlapping() {
        let a = make_part("a.rs", 1, 10, "s1", 0);
        let b = make_part("a.rs", 20, 30, "s2", 0);
        let iou = range_iou(&a, &b);
        assert_eq!(iou, 0.0);
    }

    #[test]
    fn iou_partial_overlap() {
        // [1,20] and [10,30]: inter = 10..20 = 11 lines, union = 1..30 = 30 lines → 11/30
        let a = make_part("a.rs", 1, 20, "s1", 0);
        let b = make_part("a.rs", 10, 30, "s2", 0);
        let iou = range_iou(&a, &b);
        let expected = 11.0 / 30.0;
        assert!((iou - expected).abs() < 1e-9, "got {iou}, expected {expected}");
    }

    #[test]
    fn iou_different_paths() {
        let a = make_part("a.rs", 1, 10, "s1", 0);
        let b = make_part("b.rs", 1, 10, "s2", 0);
        assert_eq!(range_iou(&a, &b), 0.0);
    }

    #[test]
    fn same_input_deterministic() {
        let parts = vec![
            make_part("a.rs", 1, 20, "s1", 0),
            make_part("a.rs", 10, 30, "s2", 0),
            make_part("b.rs", 1, 10, "s3", 0),
        ];
        let idx1 = build_canonical_ranges(&parts, &cfg());
        let idx2 = build_canonical_ranges(&parts, &cfg());
        assert_eq!(idx1.ranges, idx2.ranges);
        assert_eq!(idx1.canonical_id_of, idx2.canonical_id_of);
    }

    #[test]
    fn connected_ranges_same_canonical_id() {
        // Two ranges with iou >= 0.30 → same component.
        let parts = vec![
            make_part("a.rs", 1, 20, "s1", 0),
            make_part("a.rs", 10, 30, "s2", 1),
        ];
        let idx = build_canonical_ranges(&parts, &cfg());
        let id0 = idx.canonical_id_of[&part_key(&parts[0])];
        let id1 = idx.canonical_id_of[&part_key(&parts[1])];
        assert_eq!(id0, id1, "overlapping ranges must share a canonical id");
    }

    #[test]
    fn disjoint_ranges_different_canonical_ids() {
        // [1,10] and [20,30] → no overlap → different components.
        let parts = vec![
            make_part("a.rs", 1, 10, "s1", 0),
            make_part("a.rs", 20, 30, "s2", 1),
        ];
        let idx = build_canonical_ranges(&parts, &cfg());
        let id0 = idx.canonical_id_of[&part_key(&parts[0])];
        let id1 = idx.canonical_id_of[&part_key(&parts[1])];
        assert_ne!(id0, id1);
    }

    #[test]
    fn transitive_edges_expand_component() {
        // A=[1,20], B=[10,30], C=[20,40]:
        //   iou(A,B) = 11/30 ≈ 0.37 ≥ 0.30 ✓
        //   iou(B,C) = 11/31 ≈ 0.35 ≥ 0.30 ✓
        //   iou(A,C) = 1/40 = 0.025 < 0.30 ✗  (no direct A-C edge)
        // The algorithm expands via B: all three should land in one component.
        let parts = vec![
            make_part("a.rs", 1, 20, "s1", 0),
            make_part("a.rs", 10, 30, "s2", 1),
            make_part("a.rs", 20, 40, "s3", 2),
        ];
        let idx = build_canonical_ranges(&parts, &cfg());
        let ids: Vec<usize> = parts.iter().map(|p| idx.canonical_id_of[&part_key(p)]).collect();
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
    }

    #[test]
    fn adding_unrelated_file_does_not_change_existing_ids() {
        let parts1 = vec![
            make_part("a.rs", 1, 20, "s1", 0),
            make_part("a.rs", 10, 30, "s2", 1),
        ];
        let idx1 = build_canonical_ranges(&parts1, &cfg());

        let mut parts2 = parts1.clone();
        parts2.push(make_part("b.rs", 1, 10, "s3", 2));
        let idx2 = build_canonical_ranges(&parts2, &cfg());

        // The canonical ids for the a.rs participants must be the same.
        for p in &parts1 {
            let k = part_key(p);
            assert_eq!(idx1.canonical_id_of[&k], idx2.canonical_id_of[&k]);
        }
    }
}
