//! Pair-evidence stage (Section 7 of analyze-v4.mjs).
//!
//! Builds a `PairEvidenceMap` from four channels:
//!  1. operation-window  — within-session pair within `WINDOW_OPS` ops
//!  2. locator-edit-context — context atoms surrounding an anchored edit
//!  3. session-recurrence — synthetic row per extra session the pair appears in
//!  4. historical-cochange — seam; added by the `edges` stage
//!
//! Channel 5 (import-graph) remains a stub per plan scope.

use std::collections::{BTreeMap, BTreeSet};

use crate::advice::suggest::canonical::{part_key, CanonicalIndex};
use crate::advice::suggest::locator::prior_context_atoms;
use crate::advice::suggest::op_stream::OpKind;
use crate::advice::suggest::participants::Participant;
use crate::advice::suggest::{Op, SuggestConfig};

// ── Public types ──────────────────────────────────────────────────────────────

/// Technique label for one piece of evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Technique {
    OperationWindow,
    LocatorEditContext,
    SessionRecurrence,
}

/// One evidence record for a pair.
#[derive(Clone, Debug)]
pub struct EvidenceRecord {
    pub technique: Technique,
    /// Session id ("*recur*" for synthetic recurrence rows).
    pub sid: String,
    pub op_distance: u32,
    pub edit_anchored: u8,
    pub weight: f64,
}

/// Accumulated state for one pair.
#[derive(Clone, Debug)]
pub struct PairState {
    /// Canonical ids, sorted (lo, hi).
    pub canon_ids: (usize, usize),
    pub evidence: Vec<EvidenceRecord>,
    /// Sessions in which this pair appears (excludes synthetic rows).
    pub sessions: BTreeSet<String>,
    /// Count of `locator-edit-context` evidence hits.
    pub edit_hits: u32,
    pub weighted_hits: f64,
    /// Distinct technique kinds seen.
    pub kinds: BTreeSet<Technique>,
}

/// Map keyed by `(canonical_a, canonical_b)` with `a < b`.
pub type PairKey = (usize, usize);
pub type PairEvidenceMap = BTreeMap<PairKey, PairState>;

/// One session's ops and participants, passed into `build_pair_evidence`.
pub struct SessionParticipants {
    pub sid: String,
    pub ops: Vec<Op>,
    pub parts: Vec<Participant>,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Record one evidence item into `pairs`.
fn record(
    pairs: &mut PairEvidenceMap,
    a: usize,
    b: usize,
    ev: EvidenceRecord,
) {
    if a == b {
        return;
    }
    let key = if a < b { (a, b) } else { (b, a) };
    let (lo, hi) = key;
    let entry = pairs.entry(key).or_insert_with(|| PairState {
        canon_ids: (lo, hi),
        evidence: Vec::new(),
        sessions: BTreeSet::new(),
        edit_hits: 0,
        weighted_hits: 0.0,
        kinds: BTreeSet::new(),
    });
    entry.kinds.insert(ev.technique.clone());
    if ev.sid != "*recur*" {
        entry.sessions.insert(ev.sid.clone());
    }
    entry.weighted_hits += ev.weight;
    if ev.technique == Technique::LocatorEditContext {
        entry.edit_hits += 1;
    }
    entry.evidence.push(ev);
}

/// Compute IoU between two ranges (path must already match).
fn range_iou_raw(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> f64 {
    let lo = a_start.max(b_start);
    let hi = a_end.min(b_end);
    if hi < lo {
        return 0.0;
    }
    let inter = (hi - lo + 1) as f64;
    let a_len = (a_end - a_start + 1) as f64;
    let b_len = (b_end - b_start + 1) as f64;
    inter / (a_len + b_len - inter)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build pair evidence from all sessions.
///
/// Ports `buildPairEvidence` from `docs/analyze-v4.mjs` line 360.
pub fn build_pair_evidence(
    sessions: &[SessionParticipants],
    canonical: &CanonicalIndex,
    cfg: &SuggestConfig,
) -> PairEvidenceMap {
    let mut pairs: PairEvidenceMap = BTreeMap::new();

    for s in sessions {
        // Sort participants by op_index for window computation.
        let mut parts_sorted: Vec<&Participant> = s.parts.iter().collect();
        parts_sorted.sort_by_key(|p| p.op_index);

        // Channel 1: operation-window — sliding-window cooccurrence.
        let window_ops = cfg.window_ops as usize;
        let edit_weight_bump = cfg.edit_weight_bump;

        for (i, &a) in parts_sorted.iter().enumerate() {
            let a_id = match canonical.canonical_id_of.get(&part_key(a)) {
                Some(&id) => id,
                None => continue,
            };
            for b in parts_sorted.iter().skip(i + 1) {
                let b = *b;
                let dist = b.op_index.saturating_sub(a.op_index);
                if dist > window_ops {
                    break;
                }
                // Skip same exact anchor on same path.
                if a.path == b.path && a.m_start == b.m_start && a.m_end == b.m_end {
                    continue;
                }
                let b_id = match canonical.canonical_id_of.get(&part_key(b)) {
                    Some(&id) => id,
                    None => continue,
                };
                let has_edit = a.kind == crate::advice::suggest::participants::ParticipantKind::Edit
                    || b.kind == crate::advice::suggest::participants::ParticipantKind::Edit;
                record(
                    &mut pairs,
                    a_id,
                    b_id,
                    EvidenceRecord {
                        technique: Technique::OperationWindow,
                        sid: s.sid.clone(),
                        op_distance: dist as u32,
                        edit_anchored: if has_edit { 1 } else { 0 },
                        weight: if has_edit { edit_weight_bump } else { 1.0 },
                    },
                );
            }
        }

        // Channel 2: locator-edit-context — prior-context bag for each anchored edit.
        let iou_threshold = cfg.range_overlap_iou;
        for op in &s.ops {
            if op.kind != OpKind::Edit || op.inferred_start.is_none() {
                continue;
            }
            // Find the participant corresponding to this edit op.
            let edit_part = s.parts.iter().find(|p| p.op_index == op.op_index);
            let edit_part = match edit_part {
                Some(ep) => ep,
                None => continue,
            };
            let edit_id = match canonical.canonical_id_of.get(&part_key(edit_part)) {
                Some(&id) => id,
                None => continue,
            };
            let ctx = prior_context_atoms(&s.ops, op.op_index, cfg);
            let ctx_k = cfg.locator_prior_context_k as usize;
            for c in ctx.iter().take(ctx_k) {
                // Find a participant that matches the context atom via IoU.
                let match_part = s.parts.iter().find(|p| {
                    p.path == c.path
                        && range_iou_raw(p.m_start, p.m_end, c.start, c.end) >= iou_threshold
                });
                let ctx_part = match match_part {
                    Some(mp) => mp,
                    None => continue,
                };
                let ctx_id = match canonical.canonical_id_of.get(&part_key(ctx_part)) {
                    Some(&id) => id,
                    None => continue,
                };
                if ctx_id == edit_id {
                    continue;
                }
                record(
                    &mut pairs,
                    edit_id,
                    ctx_id,
                    EvidenceRecord {
                        technique: Technique::LocatorEditContext,
                        sid: s.sid.clone(),
                        op_distance: (op.op_index.saturating_sub(c.op_index)) as u32,
                        edit_anchored: 1,
                        weight: edit_weight_bump,
                    },
                );
            }
        }
    }

    // Channel 3: session-recurrence — synthetic evidence per extra session.
    for state in pairs.values_mut() {
        if state.sessions.len() >= 2 {
            let extra = state.sessions.len() - 1;
            for _ in 0..extra {
                state.evidence.push(EvidenceRecord {
                    technique: Technique::SessionRecurrence,
                    sid: "*recur*".to_string(),
                    op_distance: 0,
                    edit_anchored: 0,
                    weight: 1.0,
                });
                state.kinds.insert(Technique::SessionRecurrence);
                // weighted_hits already accumulated above; recurrence is synthetic
            }
        }
    }

    pairs
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::canonical::build_canonical_ranges;
    use crate::advice::suggest::op_stream::{Op, OpKind};
    use crate::advice::suggest::participants::{Participant, ParticipantKind};
    use crate::advice::suggest::SuggestConfig;

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
        // build minimal ops to match participants
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

    #[test]
    fn window_pair_different_files_produces_evidence() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());
        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        assert_eq!(pairs.len(), 1, "should produce one pair");
        let state = pairs.values().next().unwrap();
        assert!(
            state.evidence.iter().any(|e| e.technique == Technique::OperationWindow),
            "evidence should contain operation-window"
        );
    }

    #[test]
    fn same_pair_two_sessions_produces_recurrence() {
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
        assert!(
            state.evidence.iter().any(|e| e.technique == Technique::SessionRecurrence),
            "two sessions must produce session-recurrence evidence"
        );
    }

    #[test]
    fn pair_beyond_window_not_recorded() {
        // With window_ops=5, distance=6 must NOT be recorded.
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("b.rs", 1, 20, "s1", 6); // op_index 6, dist > 5
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());
        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        assert!(pairs.is_empty(), "pair beyond window should not be recorded");
    }

    #[test]
    fn same_path_same_range_not_recorded() {
        let p_a = make_part("a.rs", 1, 20, "s1", 0);
        let p_b = make_part("a.rs", 1, 20, "s1", 1);
        let all_parts = vec![p_a.clone(), p_b.clone()];
        let canonical = build_canonical_ranges(&all_parts, &cfg());
        let sessions = vec![make_session("s1", all_parts)];
        let pairs = build_pair_evidence(&sessions, &canonical, &cfg());
        assert!(pairs.is_empty(), "same path+anchor should not produce a pair");
    }
}
