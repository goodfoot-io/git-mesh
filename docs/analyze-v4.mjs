#!/usr/bin/env node
// Mesh-candidate detector v4 — scoring-focused.
//
// This file is structured for a future Rust port. Each algorithm is a single,
// pure function with explicit inputs and outputs; there is no hidden state.
// The pipeline orchestrator at the bottom reads and writes JSONL; the
// algorithms themselves do not touch IO.
//
// The single architectural choice that matters most:
//   Content cohesion is evaluated at THREE granularities — per-edge,
//   clique-pairwise-minimum, and clique-intersection — and a clique passes
//   the cohesion gate when ANY of the three holds. This fixes the v3
//   failure mode where a real n-ary mesh dies because the n-way intersection
//   of identifiers is empty even though every constituent pair shares
//   strong tokens.
//
// All other generalizations described in the v4 design plan that are
// orthogonal to scoring (output formats, cross-repo aggregation, repo
// profile files, multi-VCS) are intentionally omitted from this file.
//
// No time deltas appear in any consolidation step. Timestamps are used
// only to order events and to detect exact-equal-ts tree-diff dumps.
//
// Usage:
//   node analyze-v4.mjs [--top N] [--min-score X] [--no-trigram] [--no-history]

import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

// ============================================================================
// SECTION 1: CONFIG (constants only; no logic)
// ============================================================================

const ADVICE_BASE = process.env.GIT_MESH_ADVICE_DIR || '/tmp/git-mesh/advice';
const REPO_KEY = process.env.REPO_KEY || 'f1648e708ba2db28';
const REPO_ROOT = argFlag('--repo-root', '/workspace');
const TOP_N = Number(argFlag('--top', '40'));
const MIN_SCORE = Number(argFlag('--min-score', '0.0'));
const TRIGRAM_ENABLED = !process.argv.includes('--no-trigram');
const HISTORY_ENABLED = !process.argv.includes('--no-history');

// op-stream
const WINDOW_OPS = 5;
const LOCATOR_WINDOW = 6;
const LOCATOR_DIR_PENALTY = 0.4;
const LOCATOR_PRIOR_CONTEXT_K = 4;
const RANGE_MERGE_TOLERANCE = 5;
const RANGE_OVERLAP_IOU = 0.30;
const TREE_DIFF_BURST = 3;
const EDIT_WEIGHT_BUMP = 1.25;

// scoring + viability
const MAX_SAME_FILE_DOMINANCE = 0.66;
const SPRAWL_OP_DISTANCE_AVG = 4;
const PAIR_COHESION_FLOOR = 0.30;          // per-pair shared-id-weight floor
const CLIQUE_COHESION_FLOOR = 0.30;        // pairwise-min OR intersection floor
const PAIR_ESCAPE_BONUS = 0.20;            // pair must beat container by this to survive

// edge-score floor: drops the lowest-scoring edges before clique enumeration
// so a marginal "co-attended but barely related" edge cannot bridge two
// otherwise-distinct cliques. Setting this to 0.40 (rather than 0.30) is the
// difference between v4 absorbing the canonical 3-mesh into a 4-clique and
// surfacing it as a clean n-ary recommendation.
const EDGE_SCORE_FLOOR = 0.40;
const MAX_CLIQUE_SIZE = 8;

// history
const HISTORY_RECENCY_COMMITS = 500;
const HISTORY_HALF_LIFE_COMMITS = 200;     // exponential-decay half-life
const HISTORY_SATURATION = 4;
const HISTORY_MASS_REFACTOR_DEFAULT = 12;  // overridden by p90 of repo's commits

// IDF / shared-identifier
const SHARED_ID_SATURATION = 6;            // weight saturates here (≈6 IDF units)

const BANDS = ['low', 'medium', 'high', 'high+'];

function argFlag(name, fallback) {
  const i = process.argv.indexOf(name);
  if (i === -1) return fallback;
  return process.argv[i + 1] ?? fallback;
}

// ============================================================================
// SECTION 2: INGEST  (raw file → in-memory event records)
// ============================================================================

function readJsonl(filepath) {
  if (!fs.existsSync(filepath)) return [];
  return fs.readFileSync(filepath, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
}

// Path filter: drop directory listings, absolute paths, and lockfile/log noise.
// Self-bootstrap immunity is achieved by also dropping files whose basename
// matches the running script's basename (and analyzer-script siblings).
function isAcceptablePath(p) {
  if (!p || typeof p !== 'string') return false;
  if (p.startsWith('/')) return false;
  if (p.endsWith('/')) return false;
  if (!p.includes('/') && !p.includes('.')) return false;
  if (/(^|\/)yarn-validate-output\.log$/.test(p)) return false;
  if (/(^|\/)\.last-flush$/.test(p)) return false;
  if (!/\.[a-zA-Z0-9]{1,8}$/.test(p) && p.split('/').length <= 2) return false;
  const base = path.basename(p);
  if (/^analyze-v\d+\.mjs$/.test(base)) return false;
  if (/^mesh-suggestions(?:-v\d+)?\.(?:mjs|md)$/.test(base)) return false;
  return true;
}

function loadSessions(repoDir) {
  const out = [];
  if (!fs.existsSync(repoDir)) return out;
  for (const sid of fs.readdirSync(repoDir)) {
    const dir = path.join(repoDir, sid);
    if (!fs.statSync(dir).isDirectory()) continue;
    const reads = readJsonl(path.join(dir, 'reads.jsonl')).filter((r) => isAcceptablePath(r.path));
    const touches = readJsonl(path.join(dir, 'touches.jsonl')).filter((t) => isAcceptablePath(t.path));
    if (reads.length + touches.length === 0) continue;
    out.push({ sid, reads, touches });
  }
  return out;
}

// ============================================================================
// SECTION 3: OP-STREAM CONSTRUCTION  (no time deltas)
// ============================================================================
// Algorithm: build a per-session ordered stream of operations.
//   1. Drop touch-reads that mirror a Read of the same path/range exactly.
//   2. Detect tree-diff dumps: same-ts groups in reads (whole-file) or
//      touches ((0,0)) of size >= TREE_DIFF_BURST. Drop them entirely.
//   3. Sort remaining events by timestamp.
//   4. Coalesce consecutive same-path edits (op-stream adjacency only —
//      NOT a wall-clock gap test).
//   5. Assign sequential op_index.

function isRanged(e) { return Number(e.start_line) > 0 && Number(e.end_line) > 0; }
function isEdit(e)   { return Number(e.start_line) === 0 && Number(e.end_line) === 0; }

function groupBy(list, keyFn) {
  const m = new Map();
  for (const x of list) {
    const k = keyFn(x);
    if (!m.has(k)) m.set(k, []);
    m.get(k).push(x);
  }
  return m;
}

function buildOpStream(session) {
  const reads = session.reads.map((r) => ({ kind: 'read', ...r, t: new Date(r.ts).getTime() }));
  const touches = session.touches.map((t) => ({ ...t, t: new Date(t.ts).getTime() }));

  // Step 1: drop mirrored touch-reads.
  const readKeys = new Set(reads.map((r) => `${r.path}#${r.start_line}-${r.end_line}`));
  const touchesFiltered = touches.filter((t) => isEdit(t) || !readKeys.has(`${t.path}#${t.start_line}-${t.end_line}`));

  // Step 2: detect and drop tree-diff dumps.
  const dumpReads = new Set();
  for (const [, group] of groupBy(reads, (r) => r.ts)) {
    if (group.length >= TREE_DIFF_BURST && group.every((r) => r.start_line == null)) {
      group.forEach((r) => dumpReads.add(eventKey(r)));
    }
  }
  const dumpEdits = new Set();
  for (const [, group] of groupBy(touchesFiltered.filter(isEdit), (e) => e.ts)) {
    if (group.length >= TREE_DIFF_BURST) group.forEach((e) => dumpEdits.add(eventKey(e)));
  }

  // Step 3: sort.
  const evs = [];
  for (const r of reads) {
    if (dumpReads.has(eventKey(r))) continue;
    evs.push({ kind: 'read', path: r.path, start_line: r.start_line, end_line: r.end_line, ranged: isRanged(r), t: r.t });
  }
  for (const t of touchesFiltered) {
    if (isEdit(t)) {
      if (dumpEdits.has(eventKey(t))) continue;
      evs.push({ kind: 'edit', path: t.path, start_line: 0, end_line: 0, ranged: false, t: t.t });
    } else {
      evs.push({ kind: 'touch-read', path: t.path, start_line: t.start_line, end_line: t.end_line, ranged: isRanged(t), t: t.t });
    }
  }
  evs.sort((a, b) => a.t - b.t || (a.kind === 'read' ? -1 : 1));

  // Step 4: coalesce consecutive same-path edits.
  const ops = [];
  for (const e of evs) {
    const last = ops[ops.length - 1];
    if (e.kind === 'edit' && last && last.kind === 'edit' && last.path === e.path) {
      last.count = (last.count ?? 1) + 1;
      continue;
    }
    ops.push({ ...e, count: e.kind === 'edit' ? 1 : undefined });
  }
  ops.forEach((op, idx) => { op.op_index = idx; });
  return ops;
}

const eventKey = (e) => `${e.path}|${e.start_line ?? 'n'}-${e.end_line ?? 'n'}|${e.ts}`;

// ============================================================================
// SECTION 4: LOCATOR  (anchor each edit to a prior ranged read)
// ============================================================================
// Algorithm: for each edit op, find the read of the same path within
// LOCATOR_WINDOW positions that minimizes (op_distance + dir_penalty).
// Forward reads (after the edit) get a LOCATOR_DIR_PENALTY surcharge so
// backward reads are preferred at the same gap.

function attachLocators(ops) {
  for (let i = 0; i < ops.length; i++) {
    const e = ops[i];
    if (e.kind !== 'edit') continue;
    let best = null;
    const lo = Math.max(0, i - LOCATOR_WINDOW);
    const hi = Math.min(ops.length - 1, i + LOCATOR_WINDOW);
    for (let j = lo; j <= hi; j++) {
      if (j === i) continue;
      const r = ops[j];
      if (r.kind !== 'read' || r.path !== e.path || !r.ranged) continue;
      const gap = Math.abs(i - j);
      const dirPenalty = j > i ? LOCATOR_DIR_PENALTY : 0;
      const score = gap + dirPenalty;
      if (!best || score < best.score) best = { read: r, gap, score, fwd: j > i };
    }
    if (best) {
      e.inferred_start = best.read.start_line;
      e.inferred_end = best.read.end_line;
      e.locator_distance = best.gap;
      e.locator_forward = best.fwd;
    }
  }
}

// Prior context atoms — the LOCATOR_PRIOR_CONTEXT_K most recent ranged
// participants on the op-stream before a given edit. Used by the
// `locator-edit-context` evidence channel.
function priorContextAtoms(ops, editIndex, opWindow) {
  const out = [];
  for (let j = editIndex - 1; j >= 0 && editIndex - j <= opWindow; j--) {
    const op = ops[j];
    if (op.kind === 'read' && op.ranged) {
      out.push({ path: op.path, start: op.start_line, end: op.end_line, op_index: op.op_index });
    } else if (op.kind === 'edit' && op.inferred_start) {
      out.push({ path: op.path, start: op.inferred_start, end: op.inferred_end, op_index: op.op_index });
    }
  }
  return out;
}

// ============================================================================
// SECTION 5: PARTICIPANTS + RANGE MERGING
// ============================================================================
// A "participant" is a (path, range) atom with an op_index. Reads and
// touch-reads contribute when ranged; edits contribute only when locator-
// anchored.

function participants(ops) {
  const out = [];
  for (const op of ops) {
    if ((op.kind === 'read' || op.kind === 'touch-read') && op.ranged) {
      out.push({ path: op.path, start: op.start_line, end: op.end_line, op_index: op.op_index, kind: op.kind });
    } else if (op.kind === 'edit' && op.inferred_start) {
      out.push({
        path: op.path, start: op.inferred_start, end: op.inferred_end,
        op_index: op.op_index, kind: 'edit', anchored: true,
        locator_distance: op.locator_distance, locator_forward: op.locator_forward,
      });
    }
  }
  return out;
}

// Within a single session, merge near-touching ranges of the same file.
function mergeRangesPerFile(parts) {
  const byFile = groupBy(parts, (p) => p.path);
  const merged = new Map();
  for (const [p, ps] of byFile) {
    ps.sort((a, b) => a.start - b.start);
    const groups = [];
    for (const x of ps) {
      const last = groups[groups.length - 1];
      if (last && x.start <= last.end + RANGE_MERGE_TOLERANCE) {
        last.end = Math.max(last.end, x.end);
      } else {
        groups.push({ start: x.start, end: x.end });
      }
    }
    merged.set(p, groups);
  }
  return parts.map((p) => {
    const groups = merged.get(p.path);
    const g = groups.find((g) => g.start <= p.start && g.end >= p.end);
    return { ...p, m_start: g.start, m_end: g.end };
  });
}

// ============================================================================
// SECTION 6: CANONICAL RANGES  (cross-session range identity)
// ============================================================================
// Two ranges across sessions match when same path AND IoU >= threshold.
// Connected components under this relation become canonical ranges.

function rangeIoU(a, b) {
  if (a.path !== b.path) return 0;
  const lo = Math.max(a.start, b.start), hi = Math.min(a.end, b.end);
  if (hi < lo) return 0;
  const inter = hi - lo + 1;
  const aLen = a.end - a.start + 1, bLen = b.end - b.start + 1;
  return inter / (aLen + bLen - inter);
}

function buildCanonicalRanges(allParts) {
  const byFile = groupBy(allParts, (p) => p.path);
  const canonicalIdOf = new Map();
  const canonical = [];
  for (const [p, ps] of byFile) {
    ps.sort((a, b) => a.m_start - b.m_start);
    const components = [];
    const assigned = new Array(ps.length).fill(-1);
    for (let i = 0; i < ps.length; i++) {
      if (assigned[i] !== -1) continue;
      const comp = [i]; assigned[i] = components.length;
      for (let j = i + 1; j < ps.length; j++) {
        if (assigned[j] !== -1) continue;
        const inComp = comp.some((k) => rangeIoU(
          { path: p, start: ps[k].m_start, end: ps[k].m_end },
          { path: p, start: ps[j].m_start, end: ps[j].m_end },
        ) >= RANGE_OVERLAP_IOU);
        if (inComp) { comp.push(j); assigned[j] = components.length; }
      }
      components.push(comp);
    }
    for (const comp of components) {
      const lo = Math.min(...comp.map((k) => ps[k].m_start));
      const hi = Math.max(...comp.map((k) => ps[k].m_end));
      const id = canonical.length;
      canonical.push({ path: p, start: lo, end: hi });
      for (const k of comp) canonicalIdOf.set(partKey(ps[k]), id);
    }
  }
  return { canonical, canonicalIdOf };
}

const partKey = (p) => `${p.path}#${p.m_start}-${p.m_end}#${p.session_sid}#${p.op_index}`;

// ============================================================================
// SECTION 7: PAIR EVIDENCE  (five named channels)
// ============================================================================
// Channels:
//   1. operation-window     — within-session pair within K ops
//   2. locator-edit-context — context atoms surrounding an anchored edit
//   3. session-recurrence   — pair seen in >=2 distinct sessions
//   4. historical-cochange  — git-log walk pair count (added in scoreEdges)
//   5. import-graph         — static lookup (stub here; left as a Rust hook)
//
// A pair's evidence is a list of channel records; aggregate stats per pair
// are the inputs to scoreEdges.

function buildPairEvidence(sessions, canonicalIdOf) {
  const pairs = new Map();

  function record(a, b, ev) {
    if (a === b) return;
    const [lo, hi] = a < b ? [a, b] : [b, a];
    const key = `${lo},${hi}`;
    if (!pairs.has(key)) {
      pairs.set(key, {
        canon_ids: [lo, hi],
        evidence: [],
        sessions: new Set(),
        edit_hits: 0,
        weighted_hits: 0,
        kinds: new Set(),
      });
    }
    const r = pairs.get(key);
    r.evidence.push(ev);
    r.kinds.add(ev.technique);
    r.sessions.add(ev.sid);
    r.weighted_hits += ev.weight;
    if (ev.technique === 'locator-edit-context') r.edit_hits += 1;
  }

  for (const s of sessions) {
    if (!s.parts) continue;
    const partsSorted = [...s.parts].sort((a, b) => a.op_index - b.op_index);

    // Channel 1: operation-window — sliding-window cooccurrence.
    for (let i = 0; i < partsSorted.length; i++) {
      const a = partsSorted[i];
      const aId = canonicalIdOf.get(partKey(a));
      for (let j = i + 1; j < partsSorted.length; j++) {
        const b = partsSorted[j];
        const dist = b.op_index - a.op_index;
        if (dist > WINDOW_OPS) break;
        if (a.path === b.path && a.m_start === b.m_start && a.m_end === b.m_end) continue;
        const bId = canonicalIdOf.get(partKey(b));
        if (aId == null || bId == null) continue;
        const hasEdit = a.kind === 'edit' || b.kind === 'edit';
        record(aId, bId, {
          technique: 'operation-window',
          sid: s.sid,
          op_distance: dist,
          edit_anchored: hasEdit ? 1 : 0,
          weight: hasEdit ? EDIT_WEIGHT_BUMP : 1,
        });
      }
    }

    // Channel 2: locator-edit-context — prior-context bag for each anchored edit.
    for (const op of s.ops) {
      if (op.kind !== 'edit' || !op.inferred_start) continue;
      const editAtom = s.parts.find((p) => p.op_index === op.op_index);
      if (!editAtom) continue;
      const editId = canonicalIdOf.get(partKey(editAtom));
      if (editId == null) continue;
      const ctx = priorContextAtoms(s.ops, op.op_index, LOCATOR_WINDOW).slice(0, LOCATOR_PRIOR_CONTEXT_K);
      for (const c of ctx) {
        const matchPart = s.parts.find((p) => p.path === c.path && rangeIoU(
          { path: p.path, start: p.m_start, end: p.m_end },
          { path: c.path, start: c.start, end: c.end },
        ) >= RANGE_OVERLAP_IOU);
        if (!matchPart) continue;
        const ctxId = canonicalIdOf.get(partKey(matchPart));
        if (ctxId == null || ctxId === editId) continue;
        record(editId, ctxId, {
          technique: 'locator-edit-context',
          sid: s.sid,
          op_distance: op.op_index - c.op_index,
          edit_anchored: 1,
          weight: EDIT_WEIGHT_BUMP,
        });
      }
    }
  }

  // Channel 3: session-recurrence — synthetic evidence row per extra session.
  for (const r of pairs.values()) {
    if (r.sessions.size >= 2) {
      for (let k = 0; k < r.sessions.size - 1; k++) {
        r.evidence.push({ technique: 'session-recurrence', sid: '*recur*', op_distance: 0, edit_anchored: 0, weight: 1 });
        r.kinds.add('session-recurrence');
      }
    }
  }
  return pairs;
}

// ============================================================================
// SECTION 8: APRIORI SUPPORT / CONFIDENCE / LIFT
// ============================================================================
//   support(A,B)    = sessions_with_both / total_sessions
//   confidence(A,B) = max( P(A|B), P(B|A) )
//   lift(A,B)       = support / (P(A) * P(B))

function atomMarginals(sessions) {
  const atomSessions = new Map();
  for (const s of sessions) {
    const seen = new Set();
    for (const p of s.parts ?? []) if (p.canonical_id != null) seen.add(p.canonical_id);
    for (const id of seen) {
      if (!atomSessions.has(id)) atomSessions.set(id, new Set());
      atomSessions.get(id).add(s.sid);
    }
  }
  return atomSessions;
}

function aprioriStats(pair, atomSessions, totalSessions) {
  const [a, b] = pair.canon_ids;
  const sharedSessions = pair.sessions.size;
  const aSessions = (atomSessions.get(a) ?? new Set()).size;
  const bSessions = (atomSessions.get(b) ?? new Set()).size;
  const support = sharedSessions / Math.max(1, totalSessions);
  const confidence = Math.max(
    aSessions ? sharedSessions / aSessions : 0,
    bSessions ? sharedSessions / bSessions : 0,
  );
  const denom = (aSessions / totalSessions) * (bSessions / totalSessions);
  const lift = denom > 0 ? support / denom : 0;
  return { support, confidence, lift, sharedSessions };
}

// ============================================================================
// SECTION 9: HISTORICAL CO-CHANGE  (recency-decay weighted)
// ============================================================================
// Walk `git log -n N --no-merges --name-only`. Mass-refactor cap is the
// max(default, p90 of files-per-commit) for this repo — auto-tuned.
// Each pair's history score saturates at HISTORY_SATURATION pair-commits.
// Recency-weighted: weight(commit) = exp(-age_in_commits / HALF_LIFE).

function loadGitHistory(repoRoot, paths) {
  const fallback = { available: false, commitsByPath: new Map(), commitWeight: new Map(), totalCommits: 0 };
  if (!HISTORY_ENABLED || !repoRoot || paths.length === 0) return fallback;
  if (!fs.existsSync(path.join(repoRoot, '.git'))) return fallback;
  let stdout = '';
  try {
    stdout = execFileSync('git', [
      '-C', repoRoot, 'log',
      '--name-only', '--no-merges',
      `-n`, String(HISTORY_RECENCY_COMMITS),
      '--pretty=format:commit:%H',
    ], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'ignore'] });
  } catch { return fallback; }

  // First pass: parse commits and their file lists.
  const commits = [];
  let cur = null;
  for (const line of stdout.split('\n')) {
    if (line.startsWith('commit:')) {
      if (cur) commits.push(cur);
      cur = { hash: line.slice(7), files: new Set() };
      continue;
    }
    const f = line.trim();
    if (!f || !cur) continue;
    cur.files.add(f);
  }
  if (cur) commits.push(cur);

  // Auto-tune mass-refactor cap from commit-size distribution.
  const sizes = commits.map((c) => c.files.size).sort((a, b) => a - b);
  const p90 = sizes[Math.floor(sizes.length * 0.9)] ?? HISTORY_MASS_REFACTOR_DEFAULT;
  const massRefactorCap = Math.max(HISTORY_MASS_REFACTOR_DEFAULT, Math.min(p90, 20));

  // Second pass: build per-path commit sets and recency-decay weights.
  const wanted = new Set(paths);
  const commitsByPath = new Map(paths.map((p) => [p, new Set()]));
  const commitWeight = new Map();
  let totalKept = 0;
  // Index 0 is the most recent commit (git log order).
  for (let i = 0; i < commits.length; i++) {
    const c = commits[i];
    if (c.files.size > massRefactorCap) continue;
    if (c.files.size === 0) continue;
    const w = Math.exp(-i / HISTORY_HALF_LIFE_COMMITS);
    commitWeight.set(c.hash, w);
    totalKept++;
    for (const f of c.files) if (wanted.has(f)) commitsByPath.get(f).add(c.hash);
  }
  return { available: true, commitsByPath, commitWeight, totalCommits: totalKept, massRefactorCap };
}

function pairHistoryScore(history, pa, pb) {
  if (!history.available) return { count: 0, weighted: 0 };
  const A = history.commitsByPath.get(pa) ?? new Set();
  const B = history.commitsByPath.get(pb) ?? new Set();
  let count = 0, weighted = 0;
  for (const x of A) if (B.has(x)) {
    count++;
    weighted += history.commitWeight.get(x) ?? 0;
  }
  return { count, weighted };
}

// ============================================================================
// SECTION 10: EDGE SCORING (composite per pair)
// ============================================================================

function scoreEdges(pairs, sessions, canonical, atomSessions, history) {
  const totalSessions = sessions.length || 1;
  const edges = [];
  for (const pair of pairs.values()) {
    const [a, b] = pair.canon_ids;
    const aRange = canonical[a], bRange = canonical[b];
    if (!aRange || !bRange || aRange.path === bRange.path) continue;

    const { support, confidence, lift, sharedSessions } = aprioriStats(pair, atomSessions, totalSessions);

    const opDistances = pair.evidence
      .filter((e) => e.technique === 'operation-window' || e.technique === 'locator-edit-context')
      .map((e) => e.op_distance)
      .filter(Number.isFinite);
    const meanDistance = opDistances.length === 0
      ? WINDOW_OPS
      : opDistances.reduce((s, x) => s + x, 0) / opDistances.length;

    const hist = pairHistoryScore(history, aRange.path, bRange.path);

    // Component scores in [0,1].
    const S_recurrence = Math.min(sharedSessions / 2, 1);
    const S_confidence = Math.min(confidence, 1);
    const S_lift = Math.min(Math.log2(Math.max(lift, 1)), 3) / 3;
    const S_distance = 1 - Math.min(meanDistance, WINDOW_OPS) / (WINDOW_OPS + 1);
    const S_edit = Math.min(pair.edit_hits, 3) / 3;
    const S_kind = Math.min(pair.kinds.size, 4) / 4; // four channels available pre-history
    const S_history = history.available
      ? Math.min(hist.weighted, HISTORY_SATURATION) / HISTORY_SATURATION
      : 0.5;

    // Weighted composite. Weights sum to 1.
    const score =
      0.18 * S_recurrence +
      0.14 * S_confidence +
      0.10 * S_lift +
      0.14 * S_distance +
      0.12 * S_edit +
      0.10 * S_kind +
      0.10 * S_history +
      // remaining 0.12 slot is taken by pair content cohesion (added below).
      0;

    edges.push({
      a, b,
      sessions: sharedSessions,
      shared_sessions: [...pair.sessions],
      mean_op_distance: meanDistance,
      lift, confidence, support,
      edit_hits: pair.edit_hits,
      weighted_hits: pair.weighted_hits,
      kinds: [...pair.kinds].sort(),
      history_pair_commits: hist.count,
      history_weighted: hist.weighted,
      components: { S_recurrence, S_confidence, S_lift, S_distance, S_edit, S_kind, S_history },
      score_pre_content: score,
    });
  }
  return edges;
}

// ============================================================================
// SECTION 11: CONTENT COHESION  (three granularities)
// ============================================================================
// 1. Per-edge: cluster the two ranges' identifiers, IDF-weighted.
// 2. Clique pairwise-min: min over constituent pairs' shared-id-weight.
// 3. Clique intersection: identifiers in EVERY range, IDF-weighted.
// A clique passes the cohesion gate when ANY of (2) or (3) crosses the
// threshold. This is the v4 fix: per-edge richness translates to a strong
// pairwise-min even when the strict n-way intersection is empty.

const KEYWORDS = new Set([
  'fn','let','mut','pub','use','mod','self','Self','super','crate','struct','enum','impl','trait','where','as','in','if','else',
  'match','for','while','loop','return','break','continue','true','false','None','Some','Ok','Err','Result','Option','String','str','usize','isize',
  'u8','u16','u32','u64','i8','i16','i32','i64','bool','Vec','Box','Arc','Rc','PathBuf','Path','HashMap','HashSet','BTreeMap','and','or','not','await',
  'async','dyn','ref','static','const','echo','set','done','then','esac','case','function','var','this','new','class','export','import','from','default',
  'extends','implements','interface','type','void','null','undefined','number','string','boolean','object','any','http','https','com','org','www',
  'TODO','FIXME','XXX','NOTE','the','and','for','with','when','this','that','from','into','has','have','are','was','were','been','being',
]);

function readRange(repoRoot, p, start, end) {
  const fp = path.join(repoRoot, p);
  if (!fs.existsSync(fp)) return null;
  try {
    const text = fs.readFileSync(fp, 'utf8');
    const lines = text.split('\n');
    return lines.slice(Math.max(0, start - 1), Math.min(lines.length, end)).join('\n');
  } catch { return null; }
}

function tokensOf(text) {
  if (!text) return new Set();
  const out = new Set();
  for (const m of text.matchAll(/[A-Za-z_][A-Za-z0-9_]{2,}/g)) {
    if (!KEYWORDS.has(m[0])) out.add(m[0]);
  }
  return out;
}

function trigramsOf(text) {
  if (!text) return new Set();
  const tokens = [...tokensOf(text)].sort().join(' ');
  const out = new Set();
  for (let i = 0; i + 3 <= tokens.length; i++) out.add(tokens.slice(i, i + 3));
  return out;
}

function jaccard(a, b) {
  if (a.size === 0 || b.size === 0) return 0;
  let inter = 0;
  for (const x of a) if (b.has(x)) inter++;
  return inter / (a.size + b.size - inter);
}

function buildIdf(rangeTokens) {
  const df = new Map();
  for (const r of rangeTokens) for (const t of r.identifiers) df.set(t, (df.get(t) ?? 0) + 1);
  const N = rangeTokens.length || 1;
  const idf = new Map();
  for (const [t, c] of df) idf.set(t, Math.log((N + 1) / (1 + c)));
  return idf;
}

// Per-edge cohesion: identifiers shared between two ranges, IDF-weighted.
function pairCohesion(tokensA, tokensB, idf) {
  if (!tokensA || !tokensB) return { weight: 0, tokens: [] };
  const inter = [...tokensA.identifiers].filter((t) => tokensB.identifiers.has(t) && t.length >= 4);
  const ranked = inter
    .map((t) => ({ t, idf: idf.get(t) ?? 0 }))
    .sort((a, b) => b.idf - a.idf);
  const weight = Math.min(1, ranked.reduce((s, r) => s + r.idf, 0) / SHARED_ID_SATURATION);
  return { weight, tokens: ranked.slice(0, 8).map((r) => r.t) };
}

// Clique intersection cohesion: identifiers in EVERY range, IDF-weighted.
function intersectionCohesion(rangeTokens, idf) {
  if (rangeTokens.length === 0) return { weight: 0, tokens: [] };
  let inter = new Set(rangeTokens[0].identifiers);
  for (let i = 1; i < rangeTokens.length; i++) {
    inter = new Set([...inter].filter((t) => rangeTokens[i].identifiers.has(t)));
  }
  const ranked = [...inter]
    .filter((t) => t.length >= 4)
    .map((t) => ({ t, idf: idf.get(t) ?? 0 }))
    .sort((a, b) => b.idf - a.idf);
  const weight = Math.min(1, ranked.reduce((s, r) => s + r.idf, 0) / SHARED_ID_SATURATION);
  return { weight, tokens: ranked.slice(0, 8).map((r) => r.t) };
}

// Clique pairwise cohesion stats. Returns the min, median, and mean of the
// per-pair cohesion weights inside the clique, plus the weakest pair for
// transparency. The min is a strict gate (no weak link allowed); the median
// is a softer gate (most pairs cohere) used as the v4 cohesion fix.
function pairwiseCohesionStats(canonIds, sourceCache, canonical, idf) {
  if (canonIds.length < 2) return { min: 0, median: 0, mean: 0, weakestPair: null };
  const weights = [];
  let weakest = null;
  let minW = Infinity;
  for (let i = 0; i < canonIds.length; i++) {
    for (let j = i + 1; j < canonIds.length; j++) {
      const ra = canonical[canonIds[i]];
      const rb = canonical[canonIds[j]];
      const ta = sourceCache.get(`${ra.path}#${ra.start}-${ra.end}`);
      const tb = sourceCache.get(`${rb.path}#${rb.start}-${rb.end}`);
      const c = pairCohesion(ta, tb, idf);
      weights.push(c.weight);
      if (c.weight < minW) { minW = c.weight; weakest = [canonIds[i], canonIds[j]]; }
    }
  }
  weights.sort((a, b) => a - b);
  const min = weights[0] ?? 0;
  const median = weights[Math.floor(weights.length / 2)] ?? 0;
  const mean = weights.reduce((s, x) => s + x, 0) / Math.max(1, weights.length);
  return { min, median, mean, weakestPair: weakest };
}

// Clique trigram cohesion: minimum pairwise Jaccard of trigram sets.
function trigramCohesion(rangeTokens) {
  if (rangeTokens.length < 2) return 0;
  let min = 1;
  for (let i = 0; i < rangeTokens.length; i++) {
    for (let j = i + 1; j < rangeTokens.length; j++) {
      const j_ = jaccard(rangeTokens[i].trigrams, rangeTokens[j].trigrams);
      if (j_ < min) min = j_;
    }
  }
  return min;
}

// ============================================================================
// SECTION 12: GRAPH CONSTRUCTION + BRON-KERBOSCH MAXIMAL CLIQUES
// ============================================================================

function buildEdgeAdjacency(edges) {
  const adj = new Map();
  for (const e of edges) {
    if (!adj.has(e.a)) adj.set(e.a, new Map());
    if (!adj.has(e.b)) adj.set(e.b, new Map());
    adj.get(e.a).set(e.b, e);
    adj.get(e.b).set(e.a, e);
  }
  return adj;
}

function connectedComponents(adj) {
  const visited = new Set();
  const out = [];
  for (const node of adj.keys()) {
    if (visited.has(node)) continue;
    const stack = [node]; const comp = [];
    while (stack.length) {
      const x = stack.pop();
      if (visited.has(x)) continue;
      visited.add(x); comp.push(x);
      for (const n of adj.get(x).keys()) if (!visited.has(n)) stack.push(n);
    }
    out.push(comp);
  }
  return out;
}

// Bron–Kerbosch with pivot. Enumerates all maximal cliques of size in [2, K].
function bronKerbosch(component, adj, maxSize) {
  const result = [];
  function bk(R, P, X) {
    if (P.size === 0 && X.size === 0) {
      if (R.length >= 2 && R.length <= maxSize) result.push([...R]);
      return;
    }
    let pivot = -1, best = -1;
    for (const u of [...P, ...X]) {
      const N = adj.get(u);
      let c = 0; for (const v of P) if (N.has(v)) c++;
      if (c > best) { best = c; pivot = u; }
    }
    const pivotN = pivot === -1 ? new Map() : adj.get(pivot);
    const candidates = [...P].filter((v) => !pivotN.has(v));
    for (const v of candidates) {
      const N = adj.get(v);
      const Pn = new Set([...P].filter((x) => N.has(x)));
      const Xn = new Set([...X].filter((x) => N.has(x)));
      R.push(v); bk(R, Pn, Xn); R.pop();
      P.delete(v); X.add(v);
    }
  }
  bk([], new Set(component), new Set());
  return result;
}

function edgesWithin(canonIds, adj) {
  const out = [];
  for (let i = 0; i < canonIds.length; i++) {
    for (let j = i + 1; j < canonIds.length; j++) {
      const e = adj.get(canonIds[i])?.get(canonIds[j]);
      if (e) out.push(e);
    }
  }
  return out;
}

// ============================================================================
// SECTION 13: CANDIDATE COMPOSITE  (clique-level scoring)
// ============================================================================

function scoreCandidate(canonIds, adj, canonical, sourceCache, idf, history) {
  const ranges = canonIds.map((id) => canonical[id]);
  const inEdges = edgesWithin(canonIds, adj);
  const pairCount = (canonIds.length * (canonIds.length - 1)) / 2;
  const density = inEdges.length / Math.max(1, pairCount);

  const sessionsAll = new Set();
  for (const e of inEdges) for (const sid of e.shared_sessions) sessionsAll.add(sid);
  const sessions = sessionsAll.size;

  const meanEdgeScore = inEdges.length
    ? inEdges.reduce((s, e) => s + e.score_pre_content, 0) / inEdges.length
    : 0;
  const meanOpDistance = inEdges.length
    ? inEdges.reduce((s, e) => s + e.mean_op_distance, 0) / inEdges.length
    : WINDOW_OPS;
  const editHits = inEdges.reduce((s, e) => s + e.edit_hits, 0);
  const techniques = new Set(inEdges.flatMap((e) => e.kinds));

  const fileCounts = new Map();
  for (const r of ranges) fileCounts.set(r.path, (fileCounts.get(r.path) || 0) + 1);
  const distinctFiles = fileCounts.size;
  const maxPathShare = Math.max(...fileCounts.values()) / ranges.length;
  const topDirs = new Set(ranges.map((r) => r.path.split('/').slice(0, 3).join('/')));
  const crossPackage = topDirs.size >= 2;

  // Four-granularity content cohesion (intersection, pairwise-min,
  // pairwise-median, trigram). The cluster-level cohesion is max of the
  // four — passes if any granularity says the clique coheres.
  const tokens = ranges.map((r) => sourceCache.get(`${r.path}#${r.start}-${r.end}`)).filter(Boolean);
  let trigram = 0, intersect = { weight: 0, tokens: [] }, pw = { min: 0, median: 0, mean: 0, weakestPair: null };
  if (tokens.length === ranges.length) {
    trigram = trigramCohesion(tokens);
    intersect = intersectionCohesion(tokens, idf);
    pw = pairwiseCohesionStats(canonIds, sourceCache, canonical, idf);
  }
  const clusterCohesion = Math.max(intersect.weight, pw.median, pw.mean);
  const displayTokens = intersect.tokens.length ? intersect.tokens : (() => {
    if (!pw.weakestPair) return [];
    const [a, b] = pw.weakestPair;
    const ra = canonical[a], rb = canonical[b];
    return pairCohesion(
      sourceCache.get(`${ra.path}#${ra.start}-${ra.end}`),
      sourceCache.get(`${rb.path}#${rb.start}-${rb.end}`),
      idf,
    ).tokens;
  })();

  // History score, averaged over constituent pairs.
  const histAvg = inEdges.length
    ? inEdges.reduce((s, e) => s + e.history_weighted, 0) / inEdges.length
    : 0;
  const histCount = inEdges.length
    ? Math.round(inEdges.reduce((s, e) => s + e.history_pair_commits, 0) / inEdges.length)
    : 0;
  const S_history = history.available
    ? Math.min(histAvg, HISTORY_SATURATION) / HISTORY_SATURATION
    : 0.5;

  // Composite.
  // Weights sum to 1.0. Per the v4 plan, no single component can carry the
  // whole score: the strongest individual contribution is 0.18.
  const diversityFactor = (distinctFiles / ranges.length) * (crossPackage ? 1.0 : 0.9);
  const composite =
    0.18 * meanEdgeScore +
    0.10 * density +
    0.10 * Math.min(sessions, 3) / 3 +
    0.08 * diversityFactor +
    0.08 * Math.min(editHits, 4) / 4 +
    0.06 * Math.min(techniques.size, 5) / 5 +
    0.10 * trigram +
    0.10 * clusterCohesion +
    0.10 * S_history +
    0.10 * Math.min(meanEdgeScore, 1); // a small bonus that re-injects per-edge strength

  return {
    canon_ids: canonIds,
    ranges,
    rangesFmt: ranges.map((r) => `${r.path}#L${r.start}-L${r.end}`),
    size: ranges.length,
    distinct_files: distinctFiles,
    sessions,
    components: {
      mean_edge_score: round(meanEdgeScore),
      density: round(density),
      diversity_factor: round(diversityFactor),
      edit_hits: editHits,
      trigram_score: round(trigram),
      intersection_cohesion: round(intersect.weight),
      pairwise_min_cohesion: round(pw.min),
      pairwise_median_cohesion: round(pw.median),
      pairwise_mean_cohesion: round(pw.mean),
      cluster_cohesion: round(clusterCohesion),
      history_score: round(S_history),
    },
    techniques: [...techniques].sort(),
    historical_pair_commits: histCount,
    historical_weighted: round(histAvg),
    same_file_dominance: round(maxPathShare),
    cross_package: crossPackage,
    op_distance_avg: round(meanOpDistance),
    shared_identifiers: displayTokens,
    composite: round(composite),
  };
}

// ============================================================================
// SECTION 14: COHESION GATE  (the v4 fix)
// ============================================================================
// A clique passes the cohesion gate when ANY of:
//   - clique pairwise-min cohesion >= CLIQUE_COHESION_FLOOR (the v4 fix)
//   - clique intersection cohesion >= CLIQUE_COHESION_FLOOR
//   - clique trigram >= 0.20
//   - >=3 corroborating channels AND historical_pair_commits >= 2
// Pairs always carry their own per-edge cohesion — no separate gate.

function passesCohesionGate(c) {
  if (c.size === 2) return true; // pairs are gated by the edge-score floor only
  // Hard floor: no zero-cohesion pair allowed. A clique with a "weak link"
  // (one pair sharing nothing) is not a real n-ary coupling no matter how
  // strong the other pairs are.
  if (c.components.pairwise_min_cohesion < 0.10) return false;
  // With the weak-link rule satisfied, a clique passes when its content
  // cohesion is strong at SOME granularity. Channel agreement and history
  // are *corroborators*, never sole evidence.
  if (c.components.intersection_cohesion >= CLIQUE_COHESION_FLOOR) return true;
  if (c.components.pairwise_median_cohesion >= CLIQUE_COHESION_FLOOR + 0.10) return true;
  if (c.components.pairwise_min_cohesion >= CLIQUE_COHESION_FLOOR + 0.20) return true;
  if (c.components.trigram_score >= 0.20) return true;
  return false;
}

// ============================================================================
// SECTION 15: BAND + VIABILITY  (decoupled per the v4 plan)
// ============================================================================

function confidenceBand(c) {
  let band;
  const s = c.composite;
  if (s >= 0.78) band = 'high+';
  else if (s >= 0.60) band = 'high';
  else if (s >= 0.42) band = 'medium';
  else band = 'low';
  // Channel-count cap (sibling's idea, generalized): 1 channel caps at medium,
  // 2 at high, ≥3 reaches high+.
  const cc = c.techniques.length;
  if (cc <= 1 && BANDS.indexOf(band) > BANDS.indexOf('medium')) band = 'medium';
  if (cc <= 2 && BANDS.indexOf(band) > BANDS.indexOf('high')) band = 'high';
  // Density penalty: a non-fully-connected n-ary clique can't be high+.
  if (c.components.density < 1 && band === 'high+') band = 'high';
  return band;
}

function viabilityLabel(c, historyAvailable) {
  const cohesionPresent =
    c.components.cluster_cohesion >= 0.20
    || c.components.trigram_score >= 0.18
    || c.size === 2; // pair cohesion already gated at the edge level
  if (c.composite >= 0.55 && (c.historical_pair_commits >= 2 || cohesionPresent)) return 'viable';
  if (c.composite >= 0.65 && c.components.density >= 0.9) return 'viable';
  if (c.composite >= 0.45 && (cohesionPresent || c.historical_pair_commits >= 1)) return 'review';
  if (c.composite >= 0.40) return 'review';
  return 'weak';
}

// ============================================================================
// SECTION 16: EMISSION  (n-ary preferred, pair escape hatch)
// ============================================================================
// Emit every cohesive clique of size >=3.  Then emit a pair only if it is
// not subsumed by any kept clique, OR its composite beats every container
// by at least PAIR_ESCAPE_BONUS.

function isSuperset(big, small) {
  if (big.length <= small.length) return false;
  const B = new Set(big);
  return small.every((x) => B.has(x));
}

function emit(candidates) {
  const cliques = candidates
    .filter((c) => c.size >= 3 && passesCohesionGate(c))
    .sort((a, b) => b.composite - a.composite);
  const pairs = candidates
    .filter((c) => c.size === 2)
    .sort((a, b) => b.composite - a.composite);

  const kept = [];
  // Step 1: drop a clique iff a strictly larger kept clique already
  // contains it (so a 4-clique suppresses a contained 3-clique).
  for (const c of cliques) {
    if (kept.some((k) => isSuperset(k.canon_ids, c.canon_ids))) continue;
    kept.push(c);
  }
  // Step 2: pair escape hatch.
  for (const p of pairs) {
    const containers = kept.filter((k) => isSuperset(k.canon_ids, p.canon_ids));
    if (containers.length === 0) { kept.push(p); continue; }
    const best = Math.max(...containers.map((c) => c.composite));
    if (p.composite >= best + PAIR_ESCAPE_BONUS) kept.push(p);
  }
  return kept;
}

// ============================================================================
// SECTION 17: PIPELINE ORCHESTRATION
// ============================================================================

function round(v) { return v == null || !Number.isFinite(v) ? v : Math.round(v * 1000) / 1000; }

function main() {
  const repoDir = path.join(ADVICE_BASE, REPO_KEY);
  const sessions = loadSessions(repoDir);
  console.error(`# loaded ${sessions.length} sessions from ${repoDir}`);

  for (const s of sessions) {
    s.ops = buildOpStream(s);
    attachLocators(s.ops);
  }

  const allParts = [];
  for (const s of sessions) {
    const ps = participants(s.ops);
    if (ps.length === 0) { s.parts = []; continue; }
    const merged = mergeRangesPerFile(ps);
    for (const p of merged) { p.session_sid = s.sid; allParts.push(p); }
    s.parts = merged;
  }

  const { canonical, canonicalIdOf } = buildCanonicalRanges(allParts);
  for (const s of sessions) for (const p of s.parts ?? []) p.canonical_id = canonicalIdOf.get(partKey(p));

  const pairs = buildPairEvidence(sessions, canonicalIdOf);
  const atomSessions = atomMarginals(sessions);
  const candidatePaths = [...new Set(canonical.map((r) => r.path))];
  const history = loadGitHistory(REPO_ROOT, candidatePaths);
  console.error(`# git history: available=${history.available} commits=${history.totalCommits ?? 0} mass-refactor-cap=${history.massRefactorCap ?? '-'}`);

  // Tokenize every canonical range once. Build IDF over the corpus.
  const sourceCache = new Map();
  const allRangeTokens = [];
  if (TRIGRAM_ENABLED) {
    for (const r of canonical) {
      const key = `${r.path}#${r.start}-${r.end}`;
      const text = readRange(REPO_ROOT, r.path, r.start, r.end);
      const entry = { identifiers: tokensOf(text), trigrams: trigramsOf(text) };
      sourceCache.set(key, entry);
      allRangeTokens.push(entry);
    }
  }
  const idf = TRIGRAM_ENABLED ? buildIdf(allRangeTokens) : new Map();

  let edges = scoreEdges(pairs, sessions, canonical, atomSessions, history);

  // Fold per-edge content cohesion into the edge score (the 0.12 slot
  // reserved in scoreEdges).
  for (const e of edges) {
    const ra = canonical[e.a], rb = canonical[e.b];
    const ta = sourceCache.get(`${ra.path}#${ra.start}-${ra.end}`);
    const tb = sourceCache.get(`${rb.path}#${rb.start}-${rb.end}`);
    const c = TRIGRAM_ENABLED ? pairCohesion(ta, tb, idf) : { weight: 0 };
    e.pair_cohesion = c.weight;
    e.score = e.score_pre_content + 0.12 * c.weight;
  }

  // Drop edges below score floor before clique enumeration. When per-edge
  // cohesion is available (trigram enabled), also require minimum cohesion:
  // an edge with literally zero shared content is "co-attention without
  // semantic coupling" and should not bridge cliques.
  const passingEdges = edges.filter((e) => {
    if (e.score < EDGE_SCORE_FLOOR) return false;
    if (TRIGRAM_ENABLED && (e.pair_cohesion ?? 0) < 0.10) return false;
    return true;
  });

  const adj = buildEdgeAdjacency(passingEdges);
  const components = connectedComponents(adj);

  const allCliques = [];
  for (const comp of components) {
    if (comp.length < 2) continue;
    for (const cl of bronKerbosch(comp, adj, MAX_CLIQUE_SIZE)) allCliques.push(cl);
    // include each pair as a size-2 candidate even when subsumed.
    for (const e of passingEdges) if (comp.includes(e.a) && comp.includes(e.b)) allCliques.push([e.a, e.b]);
  }
  // Dedupe.
  const seen = new Set();
  const uniqueCliques = [];
  for (const cl of allCliques) {
    const key = [...cl].sort((a, b) => a - b).join(',');
    if (seen.has(key)) continue;
    seen.add(key);
    uniqueCliques.push([...cl].sort((a, b) => a - b));
  }

  let scored = uniqueCliques.map((ids) => scoreCandidate(ids, adj, canonical, sourceCache, idf, history));

  scored = scored.filter((c) =>
    c.composite >= MIN_SCORE
    && c.distinct_files >= 2
    && c.same_file_dominance <= MAX_SAME_FILE_DOMINANCE
    && c.op_distance_avg <= SPRAWL_OP_DISTANCE_AVG,
  );

  // Annotate confidence + viability.
  for (const c of scored) {
    c.confidence = confidenceBand(c);
    c.viability = viabilityLabel(c, history.available);
  }
  scored = scored.filter((c) => c.viability !== 'weak');

  const kept = emit(scored);
  kept.sort((a, b) =>
    BANDS.indexOf(b.confidence) - BANDS.indexOf(a.confidence)
    || b.composite - a.composite
    || b.size - a.size
    || b.sessions - a.sessions);

  for (const c of kept.slice(0, TOP_N)) console.log(JSON.stringify(c));
  console.error(`# scored=${scored.length} kept=${kept.length} output=${Math.min(kept.length, TOP_N)}`);
}

main();
