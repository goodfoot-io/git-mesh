//! Locator stage (Section 4 of analyze-v4.mjs).
//!
//! For each Edit op, find the ranged Read of the same path within
//! `locator_window` positions that minimises (op_distance + dir_penalty),
//! then annotate the Edit with the inferred anchor.

use crate::advice::suggest::op_stream::{Op, OpKind};
use crate::advice::suggest::SuggestConfig;

// ── Public types ──────────────────────────────────────────────────────────────

/// A prior-context atom: a (path, anchor) pair from an op before the current
/// edit, used by the `locator-edit-context` evidence channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub op_index: usize,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Attach locator annotations to Edit ops in-place.
///
/// Ports `attachLocators` from `docs/analyze-v4.mjs` line 210.
pub fn attach_locators(ops: &mut [Op], cfg: &SuggestConfig) {
    let window = cfg.locator_window as usize;
    let dir_penalty = cfg.locator_dir_penalty;

    // Iterate with indices; we need to look backward and forward into `ops`.
    // Because we only write to ops[i] but read ops[j] (j != i), and the read
    // fields are never the ones we write, we can split the borrow with
    // index-based access.
    let n = ops.len();
    for i in 0..n {
        if ops[i].kind != OpKind::Edit {
            continue;
        }
        let edit_path = ops[i].path.clone();

        let lo = i.saturating_sub(window);
        let hi = (i + window).min(n - 1);

        // Find the best candidate ranged Read of the same path.
        struct Best {
            start_line: u32,
            end_line: u32,
            gap: u32,
            score: f64,
            fwd: bool,
        }
        let mut best: Option<Best> = None;

        for (j, r) in ops.iter().enumerate().take(hi + 1).skip(lo) {
            if j == i {
                continue;
            }
            if r.kind != OpKind::Read || r.path != edit_path || !r.ranged {
                continue;
            }
            let (start_line, end_line) = match (r.start_line, r.end_line) {
                (Some(s), Some(e)) => (s, e),
                _ => continue,
            };
            let gap = (i as isize - j as isize).unsigned_abs() as u32;
            let fwd = j > i;
            let score = gap as f64 + if fwd { dir_penalty } else { 0.0 };
            let better = match &best {
                None => true,
                Some(b) => score < b.score,
            };
            if better {
                best = Some(Best { start_line, end_line, gap, score, fwd });
            }
        }

        if let Some(b) = best {
            ops[i].inferred_start = Some(b.start_line);
            ops[i].inferred_end = Some(b.end_line);
            ops[i].locator_distance = Some(b.gap);
            ops[i].locator_forward = Some(b.fwd);
        }
    }
}

/// Return the `locator_prior_context_k` most recent ranged-participant ops
/// before `edit_index`.
///
/// Ports `priorContextAtoms` from `docs/analyze-v4.mjs` line 238.
pub fn prior_context_atoms(ops: &[Op], edit_index: usize, cfg: &SuggestConfig) -> Vec<Atom> {
    let op_window = cfg.locator_prior_context_k as usize;
    let mut out = Vec::new();
    let lower = edit_index.saturating_sub(op_window);
    // Iterate from just before edit backwards.
    for j in (lower..edit_index).rev() {
        let op = &ops[j];
        if op.kind == OpKind::Read && op.ranged {
            if let (Some(start), Some(end)) = (op.start_line, op.end_line) {
                out.push(Atom { path: op.path.clone(), start, end, op_index: op.op_index });
            }
        } else if op.kind == OpKind::Edit
            && let (Some(start), Some(end)) = (op.inferred_start, op.inferred_end)
        {
            out.push(Atom { path: op.path.clone(), start, end, op_index: op.op_index });
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::suggest::op_stream::OpKind;

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

    #[test]
    fn edit_attaches_to_nearest_prior_read() {
        // [read A at 1..5, read B at 10..20, edit C] → attaches to B (gap 1 vs 2)
        let mut ops = vec![
            make_read("foo.rs", 1, 5, 0),
            make_read("foo.rs", 10, 20, 1),
            make_edit("foo.rs", 2),
        ];
        attach_locators(&mut ops, &cfg());
        assert_eq!(ops[2].inferred_start, Some(10));
        assert_eq!(ops[2].inferred_end, Some(20));
        assert_eq!(ops[2].locator_distance, Some(1));
        assert_eq!(ops[2].locator_forward, Some(false));
    }

    #[test]
    fn edit_beyond_window_is_unanchored() {
        // Only reads very far away (> locator_window = 6 ops away).
        let mut ops_vec: Vec<Op> = (0..7).map(|i| make_read("foo.rs", i as u32 + 1, i as u32 + 5, i)).collect();
        ops_vec.push(make_edit("foo.rs", 7));
        // op at index 7, nearest read at index 6 → gap = 1, but let's place the
        // edit 8 positions away.
        let mut ops: Vec<Op> = vec![make_read("foo.rs", 1, 5, 0)];
        // Pad with non-matching reads
        for i in 1..8 {
            ops.push(make_read("other.rs", 1, 5, i));
        }
        ops.push(make_edit("foo.rs", 8)); // gap from idx 0 = 8 > window 6
        attach_locators(&mut ops, &cfg());
        let edit = ops.last().unwrap();
        assert!(edit.inferred_start.is_none(), "should be unanchored");
    }

    #[test]
    fn forward_read_has_penalty_applied() {
        // edit at index 0, read at index 1 (forward, gap=1, score=1+0.4=1.4)
        // read at index 0 of another path won't match; let's use edit before read.
        let mut ops = vec![
            make_edit("foo.rs", 0),
            make_read("foo.rs", 10, 20, 1),
        ];
        // Also add a backward read at gap 2 (index -2 doesn't exist, so just
        // verify forward read does get attached when it's the only candidate).
        attach_locators(&mut ops, &cfg());
        assert_eq!(ops[0].inferred_start, Some(10));
        assert_eq!(ops[0].locator_forward, Some(true));
    }

    #[test]
    fn prior_context_atoms_returns_k_most_recent() {
        let cfg = SuggestConfig { locator_prior_context_k: 2, ..Default::default() };
        let ops = vec![
            make_read("a.rs", 1, 10, 0),
            make_read("b.rs", 1, 10, 1),
            make_read("c.rs", 1, 10, 2),
            make_edit("d.rs", 3),
        ];
        // edit_index = 3, window = 2 → eligible: ops[1] and ops[2]
        let atoms = prior_context_atoms(&ops, 3, &cfg);
        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[0].path, "c.rs"); // most recent first
        assert_eq!(atoms[1].path, "b.rs");
    }

    #[test]
    fn prior_context_atoms_includes_anchored_edits() {
        let mut ops = vec![
            make_read("a.rs", 5, 15, 0),
            make_edit("a.rs", 1),
            make_edit("b.rs", 2),
        ];
        // Attach locator so ops[1] becomes anchored
        attach_locators(&mut ops, &cfg());
        // ops[1] should have inferred_start = Some(5)
        let atoms = prior_context_atoms(&ops, 2, &cfg());
        // ops[1] is anchored edit, should appear
        assert!(atoms.iter().any(|a| a.path == "a.rs"));
    }
}
