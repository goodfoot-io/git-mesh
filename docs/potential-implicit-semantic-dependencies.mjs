#!/usr/bin/env node
// Mine git history for implicit semantic dependencies.
//
// Combines several signals from the literature:
//    1. Co-change (logical coupling) at file granularity, weighted by 1/commit-size
//    2. Bug-fix-filtered co-change (higher-signal subset)
//    3. Commit-message scope/ticket clustering
//    4. Diff-hunk (line-range) co-change for fine-grained "file#L-L" pairs
//    5. Author-set divergence as an inverse signal
//    6. Association rule mining (frequent itemsets via Apriori on commit transactions)
//    7. Temporal/lagged co-change (sliding window across adjacent commits)
//    8. Branch / merge topology (pre-merge feature-branch grouping via merge commits)
//    9. Cross-language symbol co-change (identifiers extracted from diff +/− lines)
//   10. Rename and move tracking (chains via `git log --follow --name-status`)
//   11. Churn correlation (per-file weekly time series, Pearson)
//   12. Defect propagation graphs (SZZ-style blame-back from fix commits)
//   13. Reviewer overlap (best-effort; reads `gh pr list` if available, else skipped)
//
// Output: ranked groupings of file ranges that warrant manual inspection
// for implicit semantic dependencies (mesh candidates).
//
// Usage:
//   node docs/potential-implicit-semantic-dependencies.mjs [options]
//
// Options:
//   --since=<git-date>       Limit history (default: 1.year)
//   --max-commit-files=<n>   Drop commits touching more than n files (default: 40)
//   --min-support=<n>        Min co-change count to report (default: 4)
//   --min-confidence=<f>     Min P(B|A) to report (default: 0.5)
//   --top=<n>                Top N pairs/groups per section (default: 25)
//   --exclude=<glob,glob>    Comma-separated path prefixes to ignore
//   --json                   Emit machine-readable JSON instead of text
//   --window=<n>             Lagged co-change window in commits (default: 5)
//   --min-itemset=<n>        Min support for Apriori 3-itemsets (default: 3)
//   --skip=<a,b,c>           Skip techniques by number (e.g. --skip=11,13)
//   --no-gh                  Skip reviewer-overlap technique even if `gh` is on PATH

import { execFileSync } from "node:child_process";
import { argv, exit, stdout } from "node:process";

// ─── args ────────────────────────────────────────────────────────────────────

const args = Object.fromEntries(
  argv.slice(2).map((a) => {
    const [k, v] = a.replace(/^--/, "").split("=");
    return [k, v ?? true];
  }),
);

const SINCE = args.since ?? "1.year";
const MAX_COMMIT_FILES = Number(args["max-commit-files"] ?? 40);
const MIN_SUPPORT = Number(args["min-support"] ?? 4);
const MIN_CONFIDENCE = Number(args["min-confidence"] ?? 0.5);
const TOP = Number(args.top ?? 25);
const JSON_OUT = Boolean(args.json);
const WINDOW = Number(args.window ?? 5);
const MIN_ITEMSET = Number(args["min-itemset"] ?? 3);
const SKIP = new Set(
  (args.skip ? String(args.skip).split(",") : []).map((s) => Number(s)),
);
const NO_GH = Boolean(args["no-gh"]);
const enabled = (n) => !SKIP.has(n);

const DEFAULT_EXCLUDES = [
  "node_modules/",
  "target/",
  "dist/",
  "build/",
  ".yarn/",
  "yarn.lock",
  "package-lock.json",
  "Cargo.lock",
  "pnpm-lock.yaml",
];
const EXCLUDES = (args.exclude ? args.exclude.split(",") : []).concat(
  DEFAULT_EXCLUDES,
);

const isExcluded = (p) => EXCLUDES.some((e) => p.startsWith(e) || p === e);

const FIX_RE = /\b(fix(es|ed)?|bug|regression|hotfix|patch|closes? #\d+)\b/i;
const TICKET_RE = /\b([A-Z][A-Z0-9]+-\d+)\b/;
const SCOPE_RE = /^(?:feat|fix|chore|refactor|docs|test|perf|build)\(([^)]+)\)/;

// ─── git ─────────────────────────────────────────────────────────────────────

function git(args) {
  return execFileSync("git", args, {
    encoding: "utf8",
    maxBuffer: 1024 * 1024 * 512,
  });
}

// One commit per record. NUL-separated header fields, then the unified=0 diff.
// Format: <sha>\x1f<parents>\x1f<unix-ts>\x1f<author-email>\x1f<subject>\x1f<body>\x1e
function readCommits() {
  const RECORD = "\x1e";
  const FIELD = "\x1f";
  const raw = git([
    "log",
    `--since=${SINCE}`,
    "--no-merges",
    `--pretty=format:%H${FIELD}%P${FIELD}%ct${FIELD}%ae${FIELD}%s${FIELD}%b${RECORD}`,
    "--unified=0",
    "--no-color",
    "-M",
    "-C",
  ]);

  const commits = [];
  for (const chunk of raw.split(RECORD)) {
    const trimmed = chunk.replace(/^\n+/, "");
    if (!trimmed) continue;
    const headerEnd = trimmed.indexOf("\n");
    const header = headerEnd === -1 ? trimmed : trimmed.slice(0, headerEnd);
    const diff = headerEnd === -1 ? "" : trimmed.slice(headerEnd + 1);
    const [sha, parents, ts, email, subject, body] = header.split(FIELD);
    if (!sha) continue;
    commits.push({
      sha,
      parents: (parents ?? "").trim().split(/\s+/).filter(Boolean),
      ts: Number(ts ?? 0),
      email: email ?? "",
      subject: subject ?? "",
      body: body ?? "",
      ...parseDiff(diff),
    });
  }
  // Newest-first from git log; keep but provide chronological order too.
  commits.sort((a, b) => b.ts - a.ts);
  return commits;
}

// Returns { files, hunks, churn, symbols, deletedRanges }
//   churn: { path: linesAdded + linesRemoved }
//   symbols: Set of identifiers seen in +/− lines (for cross-language coupling)
//   deletedRanges: { path: [{start,end}…] } for SZZ blame-back
const ID_RE = /\b[A-Za-z_][A-Za-z0-9_]{2,}\b/g;
const COMMON_KEYWORDS = new Set([
  "the", "and", "for", "with", "from", "this", "that", "let", "var", "const",
  "function", "return", "import", "export", "use", "fn", "pub", "mut", "impl",
  "self", "Self", "None", "Some", "Ok", "Err", "true", "false", "null", "undefined",
  "string", "number", "boolean", "void", "async", "await", "type", "interface",
  "class", "struct", "enum", "trait", "mod", "match", "where", "default",
  "test", "tests", "fixture", "describe", "expect", "assert",
]);

function parseDiff(diff) {
  const files = new Set();
  const hunks = {};
  const churn = {};
  const symbols = new Set();
  const deletedRanges = {};
  let current = null;
  let oldLine = 0;
  for (const line of diff.split("\n")) {
    if (line.startsWith("diff --git ")) {
      const m = line.match(/ b\/(.+)$/);
      current = m ? m[1] : null;
      if (current && !isExcluded(current)) {
        files.add(current);
        if (!hunks[current]) hunks[current] = [];
        if (!deletedRanges[current]) deletedRanges[current] = [];
        churn[current] = churn[current] ?? 0;
      } else {
        current = null;
      }
    } else if (current && line.startsWith("@@")) {
      const m = line.match(
        /@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/,
      );
      if (m) {
        const oldStart = Number(m[1]);
        const oldLen = m[2] === undefined ? 1 : Number(m[2]);
        const newStart = Number(m[3]);
        const newLen = m[4] === undefined ? 1 : Number(m[4]);
        if (newLen > 0) {
          hunks[current].push({ start: newStart, end: newStart + newLen - 1 });
        } else {
          hunks[current].push({ start: newStart, end: newStart });
        }
        if (oldLen > 0) {
          deletedRanges[current].push({
            start: oldStart,
            end: oldStart + oldLen - 1,
          });
        }
        oldLine = oldStart;
      }
    } else if (current && (line.startsWith("+") || line.startsWith("-"))) {
      if (line.startsWith("+++") || line.startsWith("---")) continue;
      churn[current]++;
      const text = line.slice(1);
      let match;
      ID_RE.lastIndex = 0;
      while ((match = ID_RE.exec(text)) !== null) {
        const id = match[0];
        if (COMMON_KEYWORDS.has(id)) continue;
        if (id.length < 4) continue;
        // Bias toward identifiers likely to cross language boundaries:
        // PascalCase, snake_case with underscore, or SCREAMING_SNAKE.
        if (/[_A-Z]/.test(id)) symbols.add(id);
      }
    }
  }
  return {
    files: [...files],
    hunks,
    churn,
    symbols: [...symbols],
    deletedRanges,
  };
}

// ─── ranges ──────────────────────────────────────────────────────────────────

// Quantize a hunk to a coarse range bucket so small drift doesn't fragment counts.
const BUCKET = 25; // lines
function bucketize(path, hunk) {
  const bStart = Math.floor((hunk.start - 1) / BUCKET) * BUCKET + 1;
  const bEnd = bStart + BUCKET - 1;
  return `${path}#L${bStart}-L${bEnd}`;
}

// ─── coupling ────────────────────────────────────────────────────────────────

function pairKey(a, b) {
  return a < b ? `${a}\x00${b}` : `${b}\x00${a}`;
}

function coChange(commits, { itemsOf, weightOf }) {
  const itemCount = new Map(); // item → weighted commits
  const pairCount = new Map(); // "a\x00b" → weighted co-occurrences
  for (const c of commits) {
    const items = itemsOf(c);
    if (items.length < 2) {
      for (const i of items) itemCount.set(i, (itemCount.get(i) ?? 0) + 1);
      continue;
    }
    const w = weightOf(c, items);
    for (const i of items) itemCount.set(i, (itemCount.get(i) ?? 0) + w);
    for (let i = 0; i < items.length; i++) {
      for (let j = i + 1; j < items.length; j++) {
        const k = pairKey(items[i], items[j]);
        pairCount.set(k, (pairCount.get(k) ?? 0) + w);
      }
    }
  }
  return { itemCount, pairCount };
}

function rankPairs({ itemCount, pairCount }, { minSupport, minConfidence }) {
  const out = [];
  for (const [k, support] of pairCount) {
    if (support < minSupport) continue;
    const [a, b] = k.split("\x00");
    const ca = itemCount.get(a) ?? 0;
    const cb = itemCount.get(b) ?? 0;
    const confAB = ca > 0 ? support / ca : 0;
    const confBA = cb > 0 ? support / cb : 0;
    const conf = Math.max(confAB, confBA);
    if (conf < minConfidence) continue;
    // Lift = P(A∧B) / (P(A)P(B)); needs total, approximate with max(itemCount)
    out.push({ a, b, support, confAB, confBA, conf });
  }
  out.sort((x, y) => y.support * y.conf - x.support * x.conf);
  return out;
}

// ─── grouping ────────────────────────────────────────────────────────────────

// Greedy clustering: walk pairs in rank order, union into components.
function clusterPairs(pairs, maxClusterSize = 8) {
  const parent = new Map();
  const find = (x) => {
    if (!parent.has(x)) parent.set(x, x);
    while (parent.get(x) !== x) {
      parent.set(x, parent.get(parent.get(x)));
      x = parent.get(x);
    }
    return x;
  };
  const union = (a, b) => {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent.set(ra, rb);
  };
  const sizeOf = new Map();
  for (const { a, b } of pairs) {
    const ra = find(a);
    const rb = find(b);
    const sa = sizeOf.get(ra) ?? 1;
    const sb = sizeOf.get(rb) ?? 1;
    if (ra !== rb && sa + sb <= maxClusterSize) {
      union(a, b);
      sizeOf.set(find(a), sa + sb);
    }
  }
  const groups = new Map();
  const seen = new Set();
  for (const { a, b } of pairs) {
    for (const x of [a, b]) {
      if (seen.has(x)) continue;
      seen.add(x);
      const r = find(x);
      if (!groups.has(r)) groups.set(r, new Set());
      groups.get(r).add(x);
    }
  }
  return [...groups.values()].map((s) => [...s]).filter((g) => g.length >= 2);
}

// ─── commit-message clustering ───────────────────────────────────────────────

function commitClusterKey(c) {
  const m = c.subject.match(TICKET_RE) ?? c.body.match(TICKET_RE);
  if (m) return `ticket:${m[1]}`;
  const s = c.subject.match(SCOPE_RE);
  if (s) return `scope:${s[1].toLowerCase()}`;
  return null;
}

function clusterCoChange(commits) {
  const buckets = new Map(); // key → Set<file>
  for (const c of commits) {
    const key = commitClusterKey(c);
    if (!key) continue;
    if (!buckets.has(key)) buckets.set(key, new Map());
    const fileCounts = buckets.get(key);
    for (const f of c.files) fileCounts.set(f, (fileCounts.get(f) ?? 0) + 1);
  }
  const groups = [];
  for (const [key, files] of buckets) {
    const top = [...files.entries()]
      .filter(([, n]) => n >= 2)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 8);
    if (top.length >= 2) {
      groups.push({ key, files: top.map(([f, n]) => ({ file: f, count: n })) });
    }
  }
  groups.sort(
    (a, b) =>
      b.files.reduce((s, x) => s + x.count, 0) -
      a.files.reduce((s, x) => s + x.count, 0),
  );
  return groups;
}

// ─── author divergence ──────────────────────────────────────────────────────

function authorOverlap(commits, pairs) {
  const authors = new Map(); // file → Set<email>
  for (const c of commits) {
    for (const f of c.files) {
      if (!authors.has(f)) authors.set(f, new Set());
      authors.get(f).add(c.email);
    }
  }
  return pairs.map((p) => {
    const A = authors.get(p.a) ?? new Set();
    const B = authors.get(p.b) ?? new Set();
    const inter = [...A].filter((x) => B.has(x)).length;
    const union = new Set([...A, ...B]).size;
    return { ...p, jaccard: union ? inter / union : 0 };
  });
}

// ─── 6. Apriori frequent itemsets (3-itemsets only; 2-itemsets covered above) ─

function aprioriTriples(commits, minSupport) {
  // Count pairs first; only triples whose three constituent pairs are all frequent
  // can themselves be frequent (downward closure). We re-use weighted support so
  // the threshold comparison is consistent with the rest of the file.
  const pair = new Map();
  const item = new Map();
  for (const c of commits) {
    const fs = c.files;
    if (fs.length < 2) continue;
    const w = 1 / Math.log2(fs.length + 1);
    for (const f of fs) item.set(f, (item.get(f) ?? 0) + w);
    for (let i = 0; i < fs.length; i++) {
      for (let j = i + 1; j < fs.length; j++) {
        const k = pairKey(fs[i], fs[j]);
        pair.set(k, (pair.get(k) ?? 0) + w);
      }
    }
  }
  const freqPair = new Set(
    [...pair.entries()].filter(([, v]) => v >= minSupport).map(([k]) => k),
  );
  // Generate candidate triples and count.
  const triple = new Map();
  for (const c of commits) {
    const fs = c.files.filter((f) => item.get(f) >= minSupport);
    if (fs.length < 3) continue;
    const w = 1 / Math.log2(c.files.length + 1);
    for (let i = 0; i < fs.length; i++) {
      for (let j = i + 1; j < fs.length; j++) {
        if (!freqPair.has(pairKey(fs[i], fs[j]))) continue;
        for (let k = j + 1; k < fs.length; k++) {
          if (!freqPair.has(pairKey(fs[i], fs[k]))) continue;
          if (!freqPair.has(pairKey(fs[j], fs[k]))) continue;
          const t = [fs[i], fs[j], fs[k]].sort().join("\x00");
          triple.set(t, (triple.get(t) ?? 0) + w);
        }
      }
    }
  }
  return [...triple.entries()]
    .filter(([, v]) => v >= minSupport)
    .map(([k, v]) => ({ items: k.split("\x00"), support: v }))
    .sort((a, b) => b.support - a.support);
}

// ─── 7. Temporal / lagged co-change ─────────────────────────────────────────

function laggedCoChange(commits, windowSize, minSupport) {
  // commits is newest-first; iterate so the window is "this commit + next N older".
  const pair = new Map();
  for (let i = 0; i < commits.length; i++) {
    const a = commits[i];
    if (a.files.length === 0 || a.files.length > MAX_COMMIT_FILES) continue;
    for (let j = i + 1; j < Math.min(commits.length, i + 1 + windowSize); j++) {
      const b = commits[j];
      if (b.files.length === 0 || b.files.length > MAX_COMMIT_FILES) continue;
      if (a.sha === b.sha) continue;
      const dt = Math.abs(a.ts - b.ts);
      if (dt > 7 * 24 * 3600) continue; // 7-day cap
      const w = 1 / (1 + Math.log2(j - i + 1));
      for (const fa of a.files) {
        for (const fb of b.files) {
          if (fa === fb) continue;
          pair.set(pairKey(fa, fb), (pair.get(pairKey(fa, fb)) ?? 0) + w);
        }
      }
    }
  }
  return [...pair.entries()]
    .filter(([, v]) => v >= minSupport)
    .map(([k, v]) => {
      const [a, b] = k.split("\x00");
      return { a, b, support: v };
    })
    .sort((x, y) => y.support - x.support);
}

// ─── 8. Branch / merge topology ─────────────────────────────────────────────

function branchTopologyGroups(minSupport) {
  // List merge commits and, for each, the files touched between the merge
  // base and the merged tip (i.e. the feature branch's contribution).
  let raw = "";
  try {
    raw = git([
      "log",
      `--since=${SINCE}`,
      "--merges",
      "--pretty=format:%H %P",
    ]);
  } catch {
    return [];
  }
  const groups = [];
  for (const line of raw.split("\n").filter(Boolean)) {
    const [merge, ...parents] = line.split(/\s+/);
    if (parents.length < 2) continue;
    const [first, ...rest] = parents;
    for (const tip of rest) {
      let base;
      try {
        base = git(["merge-base", first, tip]).trim();
      } catch {
        continue;
      }
      let names = "";
      try {
        names = git([
          "log",
          `${base}..${tip}`,
          "--pretty=format:",
          "--name-only",
        ]);
      } catch {
        continue;
      }
      const files = new Map();
      for (const f of names.split("\n").map((s) => s.trim()).filter(Boolean)) {
        if (isExcluded(f)) continue;
        files.set(f, (files.get(f) ?? 0) + 1);
      }
      const top = [...files.entries()]
        .filter(([, n]) => n >= 1)
        .sort((a, b) => b[1] - a[1])
        .slice(0, 10);
      if (top.length >= 2 && top.length >= minSupport / 2) {
        groups.push({
          merge: merge.slice(0, 8),
          tip: tip.slice(0, 8),
          files: top.map(([f, n]) => ({ file: f, count: n })),
        });
      }
    }
  }
  groups.sort(
    (a, b) =>
      b.files.reduce((s, x) => s + x.count, 0) -
      a.files.reduce((s, x) => s + x.count, 0),
  );
  return groups;
}

// ─── 9. Cross-language symbol co-change ─────────────────────────────────────

const LANG_OF = (p) => {
  const m = p.match(/\.([a-zA-Z0-9]+)$/);
  if (!m) return "?";
  const ext = m[1].toLowerCase();
  if (["ts", "tsx", "js", "jsx", "mjs", "cjs"].includes(ext)) return "ts/js";
  if (["rs"].includes(ext)) return "rust";
  if (["py"].includes(ext)) return "python";
  if (["go"].includes(ext)) return "go";
  if (["java", "kt"].includes(ext)) return "jvm";
  if (["c", "h", "cc", "cpp", "hpp"].includes(ext)) return "c/c++";
  if (["md", "txt", "rst"].includes(ext)) return "prose";
  if (["json", "yaml", "yml", "toml"].includes(ext)) return "config";
  return ext;
};

function crossLanguageSymbols(commits, minSupport) {
  // For each commit, group changed files by language; for every cross-language
  // pair of files in the same commit, record the intersection of identifiers.
  const symbolPair = new Map(); // "lang1:lang2:symbol" → { count, examples:Set<"a||b"> }
  for (const c of commits) {
    if (c.files.length < 2 || c.files.length > MAX_COMMIT_FILES) continue;
    const byLang = new Map();
    for (const f of c.files) {
      const l = LANG_OF(f);
      if (!byLang.has(l)) byLang.set(l, []);
      byLang.get(l).push(f);
    }
    if (byLang.size < 2) continue;
    const langs = [...byLang.keys()];
    const symbolsInCommit = new Set(c.symbols);
    if (symbolsInCommit.size === 0) continue;
    for (let i = 0; i < langs.length; i++) {
      for (let j = i + 1; j < langs.length; j++) {
        const la = langs[i];
        const lb = langs[j];
        const key = la < lb ? `${la}\x01${lb}` : `${lb}\x01${la}`;
        for (const sym of symbolsInCommit) {
          const k = `${key}\x01${sym}`;
          if (!symbolPair.has(k)) {
            symbolPair.set(k, { count: 0, examples: new Set() });
          }
          const e = symbolPair.get(k);
          e.count++;
          if (e.examples.size < 3) {
            const fa = byLang.get(la)[0];
            const fb = byLang.get(lb)[0];
            e.examples.add(`${fa}  ↔  ${fb}`);
          }
        }
      }
    }
  }
  return [...symbolPair.entries()]
    .filter(([, v]) => v.count >= minSupport)
    .map(([k, v]) => {
      const [la, lb, sym] = k.split("\x01");
      return {
        symbol: sym,
        langs: `${la} ↔ ${lb}`,
        count: v.count,
        examples: [...v.examples],
      };
    })
    .sort((a, b) => b.count - a.count);
}

// ─── 10. Rename / move chains ───────────────────────────────────────────────

function renameChains() {
  // Use --name-status -M -C across the whole window; gather R/C records and
  // group by similarity score buckets and original directory.
  let raw = "";
  try {
    raw = git([
      "log",
      `--since=${SINCE}`,
      "--no-merges",
      "--pretty=format:#%H",
      "--name-status",
      "-M",
      "-C",
      "--find-renames=50",
    ]);
  } catch {
    return [];
  }
  const moves = []; // { sha, from, to, score }
  let sha = null;
  for (const line of raw.split("\n")) {
    if (line.startsWith("#")) {
      sha = line.slice(1);
      continue;
    }
    const m = line.match(/^([RC])(\d+)\t(\S+)\t(\S+)$/);
    if (!m) continue;
    const [, , score, from, to] = m;
    if (isExcluded(from) || isExcluded(to)) continue;
    moves.push({ sha, from, to, score: Number(score) });
  }
  // Group renames that happened together in the same commit (a coordinated move).
  const byCommit = new Map();
  for (const r of moves) {
    if (!byCommit.has(r.sha)) byCommit.set(r.sha, []);
    byCommit.get(r.sha).push(r);
  }
  const groups = [];
  for (const [sha, rs] of byCommit) {
    if (rs.length < 2) continue;
    groups.push({ sha: sha.slice(0, 8), moves: rs });
  }
  groups.sort((a, b) => b.moves.length - a.moves.length);
  return groups;
}

// ─── 11. Churn correlation (Pearson on weekly time series) ──────────────────

function churnCorrelation(commits, minWeeksOverlap = 6, minR = 0.6) {
  // Aggregate per-file weekly churn.
  const weekOf = (ts) => Math.floor(ts / (7 * 24 * 3600));
  const series = new Map(); // file → Map<week, churn>
  for (const c of commits) {
    const w = weekOf(c.ts);
    for (const [f, n] of Object.entries(c.churn)) {
      if (!series.has(f)) series.set(f, new Map());
      const m = series.get(f);
      m.set(w, (m.get(w) ?? 0) + n);
    }
  }
  // Keep only files with enough non-zero weeks.
  const entries = [...series.entries()].filter(
    ([, m]) => [...m.values()].filter((v) => v > 0).length >= minWeeksOverlap,
  );
  const out = [];
  for (let i = 0; i < entries.length; i++) {
    for (let j = i + 1; j < entries.length; j++) {
      const [fa, ma] = entries[i];
      const [fb, mb] = entries[j];
      // Build aligned vectors over union of weeks present in either series.
      const weeks = new Set([...ma.keys(), ...mb.keys()]);
      if (weeks.size < minWeeksOverlap) continue;
      const xs = [];
      const ys = [];
      for (const w of weeks) {
        xs.push(ma.get(w) ?? 0);
        ys.push(mb.get(w) ?? 0);
      }
      const r = pearson(xs, ys);
      if (r >= minR) out.push({ a: fa, b: fb, r, weeks: weeks.size });
    }
  }
  out.sort((a, b) => b.r - a.r);
  return out;
}

function pearson(xs, ys) {
  const n = xs.length;
  if (n < 2) return 0;
  let sx = 0, sy = 0;
  for (let i = 0; i < n; i++) {
    sx += xs[i];
    sy += ys[i];
  }
  const mx = sx / n;
  const my = sy / n;
  let num = 0, dx = 0, dy = 0;
  for (let i = 0; i < n; i++) {
    const a = xs[i] - mx;
    const b = ys[i] - my;
    num += a * b;
    dx += a * a;
    dy += b * b;
  }
  const den = Math.sqrt(dx * dy);
  return den === 0 ? 0 : num / den;
}

// ─── 12. Defect propagation graph (SZZ) ─────────────────────────────────────

function defectPropagation(commits, fixCommits) {
  // For each fix commit, blame the *deleted* lines (likely the buggy code) on
  // the prior commit that introduced them. Edge: introducer → fix's other files.
  const edges = new Map(); // "introducer-file → other-file" → count
  for (const fix of fixCommits) {
    const otherFiles = new Set(fix.files);
    const introducedFiles = new Set();
    for (const [path, ranges] of Object.entries(fix.deletedRanges ?? {})) {
      for (const r of ranges) {
        // git blame the parent commit, restricting to the deleted range.
        const parent = fix.parents[0];
        if (!parent) continue;
        let blame = "";
        try {
          blame = git([
            "blame",
            "--line-porcelain",
            `-L`,
            `${r.start},${r.end}`,
            parent,
            "--",
            path,
          ]);
        } catch {
          continue;
        }
        for (const ln of blame.split("\n")) {
          const m = ln.match(/^([0-9a-f]{40}) /);
          if (m) introducedFiles.add(`${path}@${m[1].slice(0, 8)}`);
        }
      }
    }
    for (const intro of introducedFiles) {
      const introFile = intro.split("@")[0];
      for (const other of otherFiles) {
        if (other === introFile) continue;
        const k = `${introFile}\x00${other}`;
        edges.set(k, (edges.get(k) ?? 0) + 1);
      }
    }
  }
  return [...edges.entries()]
    .filter(([, v]) => v >= 2)
    .map(([k, v]) => {
      const [from, to] = k.split("\x00");
      return { from, to, count: v };
    })
    .sort((a, b) => b.count - a.count);
}

// ─── 13. Reviewer overlap (best-effort, requires `gh`) ──────────────────────

function reviewerOverlap() {
  if (NO_GH) return null;
  let json;
  try {
    const raw = git.constructor === Function
      ? execFileSync(
          "gh",
          [
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "200",
            "--json",
            "number,files,reviews",
          ],
          { encoding: "utf8", maxBuffer: 1024 * 1024 * 64 },
        )
      : "";
    json = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!Array.isArray(json) || json.length === 0) return [];
  // file → Set<reviewer>
  const reviewers = new Map();
  for (const pr of json) {
    const rs = new Set(
      (pr.reviews ?? []).map((r) => r.author?.login).filter(Boolean),
    );
    if (rs.size === 0) continue;
    for (const f of pr.files ?? []) {
      const path = f.path ?? f.filename;
      if (!path || isExcluded(path)) continue;
      if (!reviewers.has(path)) reviewers.set(path, new Set());
      for (const r of rs) reviewers.get(path).add(r);
    }
  }
  const files = [...reviewers.entries()].filter(([, s]) => s.size >= 1);
  const out = [];
  for (let i = 0; i < files.length; i++) {
    for (let j = i + 1; j < files.length; j++) {
      const [fa, ra] = files[i];
      const [fb, rb] = files[j];
      const inter = [...ra].filter((x) => rb.has(x)).length;
      const union = new Set([...ra, ...rb]).size;
      const jac = union ? inter / union : 0;
      if (jac >= 0.5 && inter >= 2) out.push({ a: fa, b: fb, inter, jaccard: jac });
    }
  }
  out.sort((a, b) => b.jaccard - a.jaccard);
  return out;
}

// ─── main ────────────────────────────────────────────────────────────────────

function fmtPair(p) {
  return `  ${p.a}\n  ${p.b}\n    support=${p.support.toFixed(2)}  conf=${p.conf.toFixed(2)}  authors∩=${p.jaccard.toFixed(2)}`;
}

function main() {
  const all = readCommits();
  const small = all.filter((c) => c.files.length <= MAX_COMMIT_FILES && c.files.length >= 2);
  const fixes = small.filter(
    (c) => FIX_RE.test(c.subject) || FIX_RE.test(c.body),
  );

  const weight = (_c, items) => 1 / Math.log2(items.length + 1);

  // 1. file-level co-change (all)
  const fileAll = rankPairs(
    coChange(small, { itemsOf: (c) => c.files, weightOf: weight }),
    { minSupport: MIN_SUPPORT, minConfidence: MIN_CONFIDENCE },
  );
  // 2. file-level co-change (bug-fixes only)
  const fileFix =
    fixes.length >= 5
      ? rankPairs(
          coChange(fixes, { itemsOf: (c) => c.files, weightOf: weight }),
          {
            minSupport: Math.max(2, Math.floor(MIN_SUPPORT / 2)),
            minConfidence: MIN_CONFIDENCE,
          },
        )
      : [];

  // 3. range-level co-change
  const rangeItemsOf = (c) => {
    const items = new Set();
    for (const [path, hs] of Object.entries(c.hunks)) {
      for (const h of hs) items.add(bucketize(path, h));
    }
    return [...items];
  };
  const rangeAll = rankPairs(
    coChange(small, { itemsOf: rangeItemsOf, weightOf: weight }),
    {
      minSupport: Math.max(3, MIN_SUPPORT - 1),
      minConfidence: MIN_CONFIDENCE,
    },
  ).filter((p) => p.a.split("#")[0] !== p.b.split("#")[0]); // cross-file only

  // 4. commit-message clusters
  const clusters = clusterCoChange(small);

  // 5. annotate file pairs with author-set jaccard
  const fileAllAnn = authorOverlap(small, fileAll.slice(0, TOP * 2));
  const fileFixAnn = authorOverlap(fixes, fileFix.slice(0, TOP * 2));

  // Group top file pairs into transitive clusters
  const fileGroups = clusterPairs(fileAll.slice(0, TOP * 4));

  // 6. Apriori 3-itemsets
  const triples = enabled(6) ? aprioriTriples(small, MIN_ITEMSET) : [];
  // 7. Lagged co-change
  const lagged = enabled(7)
    ? laggedCoChange(small, WINDOW, Math.max(2, MIN_SUPPORT - 1))
    : [];
  // 8. Branch topology
  const branches = enabled(8) ? branchTopologyGroups(MIN_SUPPORT) : [];
  // 9. Cross-language symbol co-change
  const symbols = enabled(9)
    ? crossLanguageSymbols(small, Math.max(2, MIN_SUPPORT - 1))
    : [];
  // 10. Rename / move chains
  const renames = enabled(10) ? renameChains() : [];
  // 11. Churn correlation
  const churn = enabled(11) ? churnCorrelation(small) : [];
  // 12. Defect propagation (SZZ) — bounded for performance
  const szz =
    enabled(12) && fixes.length > 0
      ? defectPropagation(small, fixes.slice(0, 50))
      : [];
  // 13. Reviewer overlap
  const reviewers = enabled(13) ? reviewerOverlap() : null;

  if (JSON_OUT) {
    stdout.write(
      JSON.stringify(
        {
          meta: {
            since: SINCE,
            commits: all.length,
            usable_commits: small.length,
            fix_commits: fixes.length,
            max_commit_files: MAX_COMMIT_FILES,
            min_support: MIN_SUPPORT,
            min_confidence: MIN_CONFIDENCE,
          },
          file_pairs: fileAllAnn.slice(0, TOP),
          fix_pairs: fileFixAnn.slice(0, TOP),
          range_pairs: rangeAll.slice(0, TOP),
          message_clusters: clusters.slice(0, TOP),
          file_groups: fileGroups.slice(0, TOP),
          apriori_triples: triples.slice(0, TOP),
          lagged_pairs: lagged.slice(0, TOP),
          branch_topology: branches.slice(0, TOP),
          cross_language_symbols: symbols.slice(0, TOP),
          rename_chains: renames.slice(0, TOP),
          churn_correlation: churn.slice(0, TOP),
          defect_propagation: szz.slice(0, TOP),
          reviewer_overlap: reviewers ? reviewers.slice(0, TOP) : null,
        },
        null,
        2,
      ) + "\n",
    );
    return;
  }

  const out = [];
  out.push(`# Potential implicit semantic dependencies`);
  out.push(``);
  out.push(
    `Scanned ${all.length} commits since ${SINCE} (${small.length} usable, ${fixes.length} bug-fixes).`,
  );
  out.push(
    `Filters: max ${MAX_COMMIT_FILES} files/commit, support≥${MIN_SUPPORT}, confidence≥${MIN_CONFIDENCE}.`,
  );
  out.push(``);
  out.push(
    `Each section lists pairs/groups that change together more often than chance would explain.`,
  );
  out.push(
    `Inspect each pair: if there's a load-bearing relationship not enforced by types/tests, it's a mesh candidate.`,
  );
  out.push(``);

  out.push(`## 1. File pairs (all commits, weighted by 1/log(commit-size))`);
  out.push(``);
  if (fileAllAnn.length === 0) out.push(`  (none above threshold)`);
  for (const p of fileAllAnn.slice(0, TOP)) out.push(fmtPair(p));
  out.push(``);

  out.push(`## 2. File pairs (bug-fix commits only — highest-signal subset)`);
  out.push(``);
  if (fileFixAnn.length === 0) out.push(`  (insufficient bug-fix commits)`);
  for (const p of fileFixAnn.slice(0, TOP)) out.push(fmtPair(p));
  out.push(``);

  out.push(
    `## 3. Cross-file range pairs (diff-hunks, ${BUCKET}-line buckets)`,
  );
  out.push(``);
  if (rangeAll.length === 0) out.push(`  (none above threshold)`);
  for (const p of rangeAll.slice(0, TOP)) out.push(fmtPair(p));
  out.push(``);

  out.push(`## 4. Commit-message clusters (ticket / conventional-commit scope)`);
  out.push(``);
  if (clusters.length === 0) out.push(`  (no recognizable scope/ticket clusters)`);
  for (const g of clusters.slice(0, TOP)) {
    out.push(`  [${g.key}]`);
    for (const f of g.files) out.push(`    ${f.file}  (×${f.count})`);
    out.push(``);
  }

  out.push(`## 5. Transitive file groups (greedy clustering of top pairs)`);
  out.push(``);
  if (fileGroups.length === 0) out.push(`  (no clusters formed)`);
  for (const g of fileGroups.slice(0, TOP)) {
    out.push(`  group:`);
    for (const f of g) out.push(`    ${f}`);
    out.push(``);
  }

  out.push(`## 6. Apriori 3-itemsets (frequent file triples)`);
  out.push(``);
  if (triples.length === 0) out.push(`  (no frequent triples above support=${MIN_ITEMSET})`);
  for (const t of triples.slice(0, TOP)) {
    out.push(`  support=${t.support.toFixed(2)}`);
    for (const f of t.items) out.push(`    ${f}`);
    out.push(``);
  }

  out.push(`## 7. Lagged co-change (window=${WINDOW} commits, ≤7 days)`);
  out.push(``);
  if (lagged.length === 0) out.push(`  (none above threshold)`);
  for (const p of lagged.slice(0, TOP)) {
    out.push(`  ${p.a}\n  ${p.b}\n    lagged-support=${p.support.toFixed(2)}`);
  }
  out.push(``);

  out.push(`## 8. Branch topology (files co-changed within feature branches)`);
  out.push(``);
  if (branches.length === 0) out.push(`  (no merge commits with multi-file branches)`);
  for (const g of branches.slice(0, TOP)) {
    out.push(`  merge=${g.merge} tip=${g.tip}`);
    for (const f of g.files) out.push(`    ${f.file}  (×${f.count})`);
    out.push(``);
  }

  out.push(`## 9. Cross-language symbol co-change`);
  out.push(``);
  if (symbols.length === 0) out.push(`  (no shared identifiers across languages)`);
  for (const s of symbols.slice(0, TOP)) {
    out.push(`  ${s.symbol}   [${s.langs}]   ×${s.count}`);
    for (const ex of s.examples) out.push(`    ${ex}`);
    out.push(``);
  }

  out.push(`## 10. Coordinated rename / move chains`);
  out.push(``);
  if (renames.length === 0) out.push(`  (no multi-file renames)`);
  for (const g of renames.slice(0, TOP)) {
    out.push(`  commit=${g.sha}  (${g.moves.length} moves)`);
    for (const m of g.moves) {
      out.push(`    [${m.score}%] ${m.from}  →  ${m.to}`);
    }
    out.push(``);
  }

  out.push(`## 11. Churn correlation (Pearson on weekly time series, r≥0.6)`);
  out.push(``);
  if (churn.length === 0) out.push(`  (no correlated pairs)`);
  for (const p of churn.slice(0, TOP)) {
    out.push(`  ${p.a}\n  ${p.b}\n    r=${p.r.toFixed(3)}  weeks=${p.weeks}`);
  }
  out.push(``);

  out.push(`## 12. Defect propagation (SZZ blame-back from fix commits)`);
  out.push(``);
  if (szz.length === 0) out.push(`  (insufficient fix data or no propagation)`);
  for (const e of szz.slice(0, TOP)) {
    out.push(`  ${e.from}  →  ${e.to}    (×${e.count} fix-pairs)`);
  }
  out.push(``);

  out.push(`## 13. Reviewer overlap`);
  out.push(``);
  if (reviewers === null) out.push(`  (gh CLI unavailable or --no-gh; skipped)`);
  else if (reviewers.length === 0) out.push(`  (no qualifying reviewer overlap)`);
  else
    for (const p of reviewers.slice(0, TOP))
      out.push(`  ${p.a}\n  ${p.b}\n    reviewers∩=${p.inter}  jaccard=${p.jaccard.toFixed(2)}`);
  out.push(``);

  out.push(`## How to act on this`);
  out.push(``);
  out.push(`For each pair/group above, open both anchors and ask:`);
  out.push(`  • Does one rely on a contract the other defines but doesn't enforce?`);
  out.push(`  • If the partner changed silently, what concrete wrong decision would I make?`);
  out.push(`  • If yes, write a mesh:  git mesh add <slug> <a> <b>  &&  git mesh why <slug> -m "..."`);
  out.push(``);
  out.push(`Inverse signal: low authors∩ jaccard on a high-confidence pair often means`);
  out.push(`the coupling is real but invisible — exactly the case meshes are designed for.`);

  stdout.write(out.join("\n") + "\n");
}

try {
  main();
} catch (e) {
  process.stderr.write(`error: ${e.message}\n`);
  exit(1);
}
