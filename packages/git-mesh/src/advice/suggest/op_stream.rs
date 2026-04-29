//! Op-stream construction (Section 3 of analyze-v4.mjs).
//!
//! Turns raw `reads.jsonl` + `touches.jsonl` for one session into an ordered,
//! deduped, coalesced sequence of `Op` records.

use std::collections::{BTreeMap, HashSet};

use crate::advice::session::state::{ReadRecord, TouchInterval};
use crate::advice::suggest::SuggestConfig;

// ── Public types ─────────────────────────────────────────────────────────────

/// The operation kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    Read,
    /// A anchor-read that came from the touches.jsonl file (start/end > 0).
    TouchRead,
    /// A whole-file edit (start_line == end_line == 0 in touches.jsonl).
    Edit,
}

/// One operation in the session op-stream.
#[derive(Clone, Debug)]
pub struct Op {
    pub path: String,
    /// `None` for Edit ops (start == 0 in source).
    pub start_line: Option<u32>,
    /// `None` for Edit ops.
    pub end_line: Option<u32>,
    /// Milliseconds-since-epoch parsed from the RFC-3339 ts field.
    pub ts_ms: i64,
    /// Sequential index assigned after coalescing (0-based).
    pub op_index: usize,
    pub kind: OpKind,
    /// True when this op carries a real line range.
    pub ranged: bool,
    /// For coalesced Edit ops: how many raw edit events were merged.
    pub count: u32,
    // Locator fields — populated by the locator stage, not op_stream.
    pub inferred_start: Option<u32>,
    pub inferred_end: Option<u32>,
    pub locator_distance: Option<u32>,
    pub locator_forward: Option<bool>,
}

/// One session's raw data, mirroring the JS `session` object.
pub struct SessionRecord {
    pub sid: String,
    pub reads: Vec<ReadRecord>,
    pub touches: Vec<TouchInterval>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse an RFC-3339 timestamp to milliseconds since epoch.
///
/// Uses a hand-rolled parser to avoid pulling in `chrono` at this stage;
/// the JS calls `new Date(ts).getTime()`.
fn parse_ts_ms(ts: &str) -> i64 {
    // Delegate to a simple approach: treat the string as an ISO-8601 date.
    // We rely on the format produced by the advice hook: "YYYY-MM-DDTHH:MM:SS.mmmZ"
    // or "YYYY-MM-DDTHH:MM:SSZ".
    //
    // Strategy: parse with the `time` crate if available, else fall back to
    // a simple seconds-based calculation.  The advice codebase already uses
    // `chrono`-compatible types in other modules, so we keep dependencies
    // minimal and use the standard library epoch trick.
    parse_rfc3339_ms(ts).unwrap_or(0)
}

/// Hand-rolled RFC-3339 to ms-since-epoch parser.
/// Handles: `YYYY-MM-DDTHH:MM:SS[.fff]Z` and `...+HH:MM` / `-HH:MM`.
fn parse_rfc3339_ms(ts: &str) -> Option<i64> {
    // Minimum length: 20 chars for "YYYY-MM-DDTHH:MM:SSZ"
    if ts.len() < 20 {
        return None;
    }
    let bytes = ts.as_bytes();
    let year: i64 = parse_digits(&bytes[0..4])?;
    let month: i64 = parse_digits(&bytes[5..7])?;
    let day: i64 = parse_digits(&bytes[8..10])?;
    let hour: i64 = parse_digits(&bytes[11..13])?;
    let min: i64 = parse_digits(&bytes[14..16])?;
    let sec: i64 = parse_digits(&bytes[17..19])?;

    // Fractional seconds
    let mut frac_ms: i64 = 0;
    let mut pos = 19usize;
    if pos < bytes.len() && bytes[pos] == b'.' {
        pos += 1;
        let frac_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        let frac_digits = &bytes[frac_start..pos];
        // take up to 3 digits for milliseconds
        let take = frac_digits.len().min(3);
        let mut ms_val: i64 = parse_digits(&frac_digits[..take])?;
        // pad to 3 digits if fewer
        for _ in take..3 {
            ms_val *= 10;
        }
        frac_ms = ms_val;
    }

    // Timezone offset in minutes
    let tz_offset_mins: i64 = if pos < bytes.len() {
        match bytes[pos] {
            b'Z' => 0,
            b'+' | b'-' => {
                let sign: i64 = if bytes[pos] == b'+' { 1 } else { -1 };
                pos += 1;
                if pos + 5 > bytes.len() {
                    return None;
                }
                let tz_h: i64 = parse_digits(&bytes[pos..pos + 2])?;
                let tz_m: i64 = parse_digits(&bytes[pos + 3..pos + 5])?;
                sign * (tz_h * 60 + tz_m)
            }
            _ => return None,
        }
    } else {
        0
    };

    // Days since epoch via Julian Day Number method
    let jdn = julian_day(year, month, day);
    let epoch_jdn = julian_day(1970, 1, 1);
    let days: i64 = jdn - epoch_jdn;

    let total_secs = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset_mins * 60;
    Some(total_secs * 1000 + frac_ms)
}

fn julian_day(y: i64, m: i64, d: i64) -> i64 {
    // Algorithm from https://en.wikipedia.org/wiki/Julian_day
    (1461 * (y + 4800 + (m - 14) / 12)) / 4 + (367 * (m - 2 - 12 * ((m - 14) / 12))) / 12
        - (3 * ((y + 4900 + (m - 14) / 12) / 100)) / 4
        + d
        - 32075
}

fn parse_digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (b - b'0') as i64;
    }
    Some(v)
}

/// Touches always represent whole-file changes attributed to one tool call.
fn is_edit_touch(_t: &TouchInterval) -> bool {
    true
}

/// Whether a ReadRecord carries a real line range.
fn is_ranged_read(r: &ReadRecord) -> bool {
    matches!((r.start_line, r.end_line), (Some(s), Some(e)) if s > 0 && e > 0)
}

/// Stable event key for dedup/dump detection, mirroring JS `eventKey`.
///
/// JS: `${e.path}|${e.start_line ?? 'n'}-${e.end_line ?? 'n'}|${e.ts}`
fn read_event_key(r: &ReadRecord) -> String {
    let s = r
        .start_line
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n".to_string());
    let e = r
        .end_line
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n".to_string());
    format!("{}|{}-{}|{}", r.path, s, e, r.ts)
}

fn touch_event_key(t: &TouchInterval) -> String {
    format!("{}|0-0|{}", t.path, t.ts)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build the op-stream for one session.
///
/// Faithfully ports `buildOpStream` from `docs/analyze-v4.mjs` line 150.
pub fn build_op_stream(session: &SessionRecord, cfg: &SuggestConfig) -> Vec<Op> {
    let tree_diff_burst = cfg.tree_diff_burst as usize;

    // ── Step 1: drop mirrored touch-reads ────────────────────────────────────
    // JS builds `readKeys` as `path#start-end` set, then filters touches that
    // are edits OR whose key is not in readKeys.
    let read_keys: HashSet<String> = session
        .reads
        .iter()
        .map(|r| {
            let s = r
                .start_line
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string());
            let e = r
                .end_line
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string());
            format!("{}#{}-{}", r.path, s, e)
        })
        .collect();

    // All touches are whole-file edits (no line-range mirror to dedup against reads).
    let _ = &read_keys;
    let touches_filtered: Vec<&TouchInterval> = session.touches.iter().collect();

    // ── Step 2: detect and drop tree-diff dumps ───────────────────────────────
    // Reads: same-ts groups of size >= TREE_DIFF_BURST where every entry has
    // start_line == null (whole-file).
    let mut dump_reads: HashSet<String> = HashSet::new();
    {
        // Group reads by ts
        let mut by_ts: BTreeMap<&str, Vec<&ReadRecord>> = BTreeMap::new();
        for r in &session.reads {
            by_ts.entry(r.ts.as_str()).or_default().push(r);
        }
        for group in by_ts.values() {
            if group.len() >= tree_diff_burst && group.iter().all(|r| r.start_line.is_none()) {
                for r in group {
                    dump_reads.insert(read_event_key(r));
                }
            }
        }
    }

    let mut dump_edits: HashSet<String> = HashSet::new();
    {
        let edit_touches: Vec<&&TouchInterval> = touches_filtered
            .iter()
            .filter(|t| is_edit_touch(t))
            .collect();
        let mut by_ts: BTreeMap<&str, Vec<&&TouchInterval>> = BTreeMap::new();
        for t in &edit_touches {
            by_ts.entry(t.ts.as_str()).or_default().push(t);
        }
        for group in by_ts.values() {
            if group.len() >= tree_diff_burst {
                for t in group {
                    dump_edits.insert(touch_event_key(t));
                }
            }
        }
    }

    // ── Step 3: sort ─────────────────────────────────────────────────────────
    // Build a unified event list.  Each entry carries: ts_ms, kind, path,
    // start_line, end_line, ranged.
    #[derive(Clone)]
    struct RawEv {
        ts_ms: i64,
        kind: OpKind,
        path: String,
        start_line: Option<u32>,
        end_line: Option<u32>,
        ranged: bool,
    }

    let mut evs: Vec<RawEv> = Vec::new();

    for r in &session.reads {
        if dump_reads.contains(&read_event_key(r)) {
            continue;
        }
        evs.push(RawEv {
            ts_ms: parse_ts_ms(&r.ts),
            kind: OpKind::Read,
            path: r.path.clone(),
            start_line: r.start_line,
            end_line: r.end_line,
            ranged: is_ranged_read(r),
        });
    }

    for t in &touches_filtered {
        if dump_edits.contains(&touch_event_key(t)) {
            continue;
        }
        evs.push(RawEv {
            ts_ms: parse_ts_ms(&t.ts),
            kind: OpKind::Edit,
            path: t.path.clone(),
            start_line: None,
            end_line: None,
            ranged: false,
        });
    }

    // Sort: ascending ts_ms; on tie reads sort before non-reads (mirrors JS
    // `a.kind === 'read' ? -1 : 1`).
    evs.sort_by(|a, b| {
        a.ts_ms.cmp(&b.ts_ms).then_with(|| {
            let a_read = a.kind == OpKind::Read;
            let b_read = b.kind == OpKind::Read;
            b_read.cmp(&a_read) // true > false, so reads sort first
        })
    });

    // ── Step 4: coalesce consecutive same-path edits ──────────────────────────
    let mut ops: Vec<Op> = Vec::new();
    for ev in evs {
        if ev.kind == OpKind::Edit
            && let Some(last) = ops.last_mut()
            && last.kind == OpKind::Edit
            && last.path == ev.path
        {
            last.count += 1;
            continue;
        }
        ops.push(Op {
            path: ev.path,
            start_line: ev.start_line,
            end_line: ev.end_line,
            ts_ms: ev.ts_ms,
            op_index: 0, // assigned below
            kind: ev.kind,
            ranged: ev.ranged,
            count: 1,
            inferred_start: None,
            inferred_end: None,
            locator_distance: None,
            locator_forward: None,
        });
    }

    // ── Step 5: assign sequential op_index ───────────────────────────────────
    for (idx, op) in ops.iter_mut().enumerate() {
        op.op_index = idx;
    }

    ops
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SuggestConfig {
        SuggestConfig::default()
    }

    fn read(path: &str, start: Option<u32>, end: Option<u32>, ts: &str) -> ReadRecord {
        ReadRecord {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            ts: ts.to_string(),
            id: None,
        }
    }

    fn touch(path: &str, _start: u32, _end: u32, ts: &str) -> TouchInterval {
        use crate::advice::session::state::TouchKind;
        TouchInterval {
            path: path.to_string(),
            kind: TouchKind::Modified,
            id: "test".to_string(),
            ts: ts.to_string(),
        }
    }

    fn session(reads: Vec<ReadRecord>, touches: Vec<TouchInterval>) -> SessionRecord {
        SessionRecord {
            sid: "test".to_string(),
            reads,
            touches,
        }
    }

    #[test]
    fn tree_diff_dump_reads_dropped() {
        // Three whole-file reads at the same timestamp → all dropped.
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
        assert!(
            ops.is_empty(),
            "dump reads should be dropped; got {:?}",
            ops.len()
        );
    }

    #[test]
    fn tree_diff_dump_edits_dropped() {
        // Three whole-file edits at the same timestamp → all dropped.
        let ts = "2024-01-01T00:00:00Z";
        let s = session(
            vec![],
            vec![
                touch("a.rs", 0, 0, ts),
                touch("b.rs", 0, 0, ts),
                touch("c.rs", 0, 0, ts),
            ],
        );
        let ops = build_op_stream(&s, &cfg());
        assert!(ops.is_empty(), "dump edits should be dropped");
    }

    #[test]
    fn ranged_read_and_whole_file_touch_both_kept() {
        // Touches are whole-file (no line range), so a same-path ranged read
        // and a whole-file edit do not mirror — both surface as ops.
        let ts = "2024-01-01T00:00:00Z";
        let s = session(
            vec![read("foo.rs", Some(10), Some(20), ts)],
            vec![touch("foo.rs", 0, 0, ts)],
        );
        let ops = build_op_stream(&s, &cfg());
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].kind, OpKind::Read);
        assert_eq!(ops[1].kind, OpKind::Edit);
    }

    #[test]
    fn consecutive_edits_same_path_coalesced() {
        let ts1 = "2024-01-01T00:00:01Z";
        let ts2 = "2024-01-01T00:00:02Z";
        let s = session(
            vec![],
            vec![touch("foo.rs", 0, 0, ts1), touch("foo.rs", 0, 0, ts2)],
        );
        let ops = build_op_stream(&s, &cfg());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].count, 2);
    }

    #[test]
    fn consecutive_edits_different_paths_not_coalesced() {
        let ts1 = "2024-01-01T00:00:01Z";
        let ts2 = "2024-01-01T00:00:02Z";
        let s = session(
            vec![],
            vec![touch("a.rs", 0, 0, ts1), touch("b.rs", 0, 0, ts2)],
        );
        let ops = build_op_stream(&s, &cfg());
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn op_indices_are_sequential() {
        let ts = "2024-01-01T00:00:01Z";
        let s = session(
            vec![read("a.rs", Some(1), Some(5), ts)],
            vec![touch("b.rs", 0, 0, ts)],
        );
        let ops = build_op_stream(&s, &cfg());
        for (i, op) in ops.iter().enumerate() {
            assert_eq!(op.op_index, i);
        }
    }

    #[test]
    fn reads_sort_before_other_kinds_at_same_ts() {
        let ts = "2024-01-01T00:00:01Z";
        let s = session(
            vec![read("a.rs", Some(1), Some(5), ts)],
            // edit at same timestamp
            vec![touch("b.rs", 0, 0, ts)],
        );
        let ops = build_op_stream(&s, &cfg());
        assert_eq!(ops[0].kind, OpKind::Read);
        assert_eq!(ops[1].kind, OpKind::Edit);
    }

    #[test]
    fn parse_ts_ms_basic() {
        // 1970-01-01T00:00:00Z → 0
        assert_eq!(parse_ts_ms("1970-01-01T00:00:00Z"), 0);
        // 1970-01-01T00:00:01Z → 1000
        assert_eq!(parse_ts_ms("1970-01-01T00:00:01Z"), 1000);
        // 1970-01-01T00:00:00.500Z → 500
        assert_eq!(parse_ts_ms("1970-01-01T00:00:00.500Z"), 500);
    }
}
