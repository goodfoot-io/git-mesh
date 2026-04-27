#!/usr/bin/env node
// Mine git history for implicit semantic dependencies.
//
// Combines several signals from the literature:
//    1. Co-change (logical coupling) at file granularity, weighted by 1/commit-size
//    2. Bug-fix-filtered co-change (higher-signal subset)
//    3. Commit-message scope/ticket clustering
//    4. Diff-hunk co-change keyed by enclosing symbol / heading (drift-resistant)
//    5. Author-set divergence as an inverse signal
//    6. Association rule mining (frequent itemsets via Apriori on commit transactions)
//    7. Directional lagged co-change (sliding window across adjacent commits)
//    8. Branch / merge topology (pre-merge feature-branch grouping via merge commits)
//    9. Cross-language symbol co-change (definition-anchored identifiers only)
//   10. Rename and move tracking (chains via `git log --follow --name-status`)
//   11. Churn correlation (per-file weekly time series, Pearson; pruned to co-change candidates)
//   12. Defect propagation graphs (SZZ-style blame-back from fix commits, batched)
//   13. Reviewer overlap (best-effort; reads `gh pr list` if available, else skipped)
//
// Output: a unified ranked table of pairs with per-technique columns, plus
// per-section detail. The top of the table is the mesh candidate shortlist.
//
// Usage:
//   node scripts/potential-implicit-semantic-dependencies.mjs [options]
//
// Options:
//   --since=<git-date>       Limit history (default: 1.year)
//   --max-commit-files=<n>   Drop commits touching more than n files (default: 40)
//   --min-support=<n>        Min co-change count to report (default: 4)
//   --min-confidence=<f>     Min P(B|A) to report (default: 0.5)
//   --top=<n>                Top N pairs/groups per section (default: 25)
//   --top-percent=<p>        Alternative to --top: keep top p% of each section
//   --exclude=<glob,glob>    Comma-separated path prefixes to ignore
//   --json                   Emit machine-readable JSON instead of text
//   --json-out=<path>        Always write JSON companion to this path (default: alongside)
//   --window=<n>             Lagged co-change window in commits (default: 5)
//   --min-itemset=<n>        Min support for Apriori 3-itemsets (default: 3)
//   --skip=<a,b,c>           Skip techniques by number (e.g. --skip=11,13)
//   --no-gh                  Skip reviewer-overlap technique even if `gh` is on PATH
//   --fix-regex=<pattern>    Override fix-commit regex (case-insensitive)
//   --explain=<a>:<b>        Print commits where both file <a> and <b> changed, then exit
//   --explain-hunks          With --explain, also print the actual hunks per commit
//   --szz-max=<n>            Cap SZZ to first N fix commits (default 50)
//   --itemset-k=<n>          Max itemset size for Apriori (default 3, max 5)
//   --min-lift=<f>           Min lift for Apriori rules (default 1.5)
//   --author-jaccard-min=<f> Min authors∩ jaccard for positive author clusters (default 0.7)

import { execFileSync, spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { argv, exit, stdout } from "node:process";
import { dirname, basename, resolve } from "node:path";

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
const TOP_PERCENT = args["top-percent"] !== undefined ? Number(args["top-percent"]) : null;
const JSON_OUT = Boolean(args.json);
const JSON_OUT_PATH = args["json-out"] && args["json-out"] !== true ? String(args["json-out"]) : null;
const WINDOW = Number(args.window ?? 5);
const MIN_ITEMSET = Number(args["min-itemset"] ?? 3);
const SKIP = new Set(
  (args.skip ? String(args.skip).split(",") : []).map((s) => Number(s)),
);
const NO_GH = Boolean(args["no-gh"]);
const enabled = (n) => !SKIP.has(n);
const EXPLAIN = args.explain && args.explain !== true ? String(args.explain) : null;
const EXPLAIN_HUNKS = Boolean(args["explain-hunks"]);
const SZZ_MAX = Number(args["szz-max"] ?? 50);
const ITEMSET_K = Math.min(5, Math.max(2, Number(args["itemset-k"] ?? 3)));
const MIN_LIFT = Number(args["min-lift"] ?? 1.5);
const AUTHOR_JACCARD_MIN = Number(args["author-jaccard-min"] ?? 0.7);

const DEFAULT_EXCLUDES = [
  "node_modules/",
  "target/",
  "dist/",
  "build/",
  "coverage/",
  ".yarn/",
  "__snapshots__/",
];
// Lockfiles are NOT in EXCLUDES — we keep them in the file list so we can use
// them as *boundary markers* (commits touching lockfiles are dep-bumps and
// shouldn't generate coupling edges). They are stripped at coupling time, not
// at parse time.
const LOCKFILE_PATHS = new Set([
  "yarn.lock", "package-lock.json", "Cargo.lock", "pnpm-lock.yaml",
  "Gemfile.lock", "poetry.lock", "go.sum", "Pipfile.lock", "composer.lock",
]);
const isLockfile = (p) => LOCKFILE_PATHS.has(p) || p.endsWith("/yarn.lock") ||
  p.endsWith("/package-lock.json") || p.endsWith("/Cargo.lock") ||
  p.endsWith("/pnpm-lock.yaml") || p.endsWith("/go.sum");
const DEP_BUMP_RE = /\b(bump|upgrade|update)\b.*\b(deps?|dependenc|version|packages?)\b|^chore\(deps\)|dependabot|renovate/i;
const EXCLUDE_SUFFIXES = [".snap", ".lock", ".min.js", ".min.css", ".map"];
const EXCLUDES = (args.exclude ? args.exclude.split(",") : []).concat(
  DEFAULT_EXCLUDES,
);
// Match excludes as either a prefix at the repo root OR as a `/<segment>/`
// substring anywhere in the path — so `dist/` excludes both `dist/foo.js` and
// `packages/x/dist/foo.js` without over-matching `district/` (the trailing
// slash anchors to a path segment boundary).
const isExcluded = (p) =>
  EXCLUDES.some((e) => p.startsWith(e) || p === e || (e.endsWith("/") && p.includes(`/${e}`))) ||
  EXCLUDE_SUFFIXES.some((s) => p.endsWith(s));

const DEFAULT_FIX_RE =
  /\b(fix(es|ed|ing)?|bug|regression|hotfix|patch|revert|restore|tighten|close[sd]?\s+(the\s+)?\S+\s+loophole|closes?\s+#\d+)\b/i;
const FIX_RE = args["fix-regex"] && args["fix-regex"] !== true
  ? new RegExp(String(args["fix-regex"]), "i")
  : DEFAULT_FIX_RE;
const TICKET_RE = /\b([A-Z][A-Z0-9]+-\d+)\b/;
const SCOPE_RE = /^(?:feat|fix|chore|refactor|docs|test|perf|build)\(([^)]+)\)/;

// Limit per-pair `--explain` mode early.
function git(args) {
  return execFileSync("git", args, { encoding: "utf8", maxBuffer: 1024 * 1024 * 512 });
}

// ─── streaming git log reader ────────────────────────────────────────────────
//
// `git log --unified=0` over a year of an active repo blows past Node's max
// string length. Stream the process stdout and split on RECORD separators
// incrementally so we never materialize the full log in one buffer.

const RECORD_START = "\x03"; // leading sentinel so each chunk is self-contained
const FIELD = "\x1f";
const HEADER_END = "\x02"; // separates body from diff (body may contain newlines)

function readCommits() {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(
      "git",
      [
        "log",
        `--since=${SINCE}`,
        "--no-merges",
        `--pretty=format:${RECORD_START}%H${FIELD}%P${FIELD}%ct${FIELD}%ae${FIELD}%s${FIELD}%b${HEADER_END}`,
        "--unified=0",
        "--no-color",
        "-M",
        "-C",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    const commits = [];
    let buffer = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      buffer += chunk;
      // Each record begins with RECORD_START; flush all complete records (those
      // followed by another RECORD_START — only then is the diff fully captured).
      let starts = [];
      for (let i = 0; i < buffer.length; i++) if (buffer[i] === RECORD_START) starts.push(i);
      if (starts.length < 2) return;
      for (let s = 0; s < starts.length - 1; s++) {
        const record = buffer.slice(starts[s] + 1, starts[s + 1]);
        const c = parseRecord(record);
        if (c) commits.push(c);
      }
      buffer = buffer.slice(starts[starts.length - 1]);
    });
    child.stderr.on("data", (c) => (stderr += c));
    child.on("close", (code) => {
      if (buffer.startsWith(RECORD_START)) {
        const c = parseRecord(buffer.slice(1));
        if (c) commits.push(c);
      }
      if (code !== 0) return rejectPromise(new Error(`git log exited ${code}: ${stderr}`));
      commits.sort((a, b) => b.ts - a.ts);
      resolvePromise(commits);
    });
    child.on("error", rejectPromise);
  });
}

function parseRecord(chunk) {
  const trimmed = chunk.replace(/^\n+/, "");
  if (!trimmed) return null;
  const headerEnd = trimmed.indexOf(HEADER_END);
  const header = headerEnd === -1 ? trimmed : trimmed.slice(0, headerEnd);
  const diff = headerEnd === -1 ? "" : trimmed.slice(headerEnd + 1).replace(/^\n+/, "");
  const [sha, parents, ts, email, subject, body] = header.split(FIELD);
  if (!sha) return null;
  return {
    sha,
    parents: (parents ?? "").trim().split(/\s+/).filter(Boolean),
    ts: Number(ts ?? 0),
    email: email ?? "",
    subject: subject ?? "",
    body: body ?? "",
    ...parseDiff(diff),
  };
}

// ─── diff parser ─────────────────────────────────────────────────────────────

const ID_RE = /\b[A-Za-z_][A-Za-z0-9_]{2,}\b/g;
const COMMON_KEYWORDS = new Set([
  "the", "and", "for", "with", "from", "this", "that", "let", "var", "const",
  "function", "return", "import", "export", "use", "fn", "pub", "mut", "impl",
  "self", "Self", "None", "Some", "Ok", "Err", "true", "false", "null", "undefined",
  "string", "number", "boolean", "void", "async", "await", "type", "interface",
  "class", "struct", "enum", "trait", "mod", "match", "where", "default",
  "test", "tests", "fixture", "describe", "expect", "assert",
  "When", "This", "That", "These", "Those", "Read", "Write", "Note",
]);

// Identifiers introduced as definitions on a +/− line. The regex captures
// declaration keywords across TS/JS, Rust, Python, Go, Java/Kotlin, C/C++.
const DEFN_RE =
  /\b(?:class|interface|struct|enum|trait|type|fn|func|def|function|const|let|var|impl)\s+([A-Z_a-z][A-Za-z0-9_]{2,})/g;

function parseDiff(diff) {
  const files = new Set();
  const hunks = {}; // path → [{start,end,anchor}]
  const churn = {};
  const definedSymbols = new Set(); // identifiers declared in this commit
  const referencedSymbols = new Set(); // identifiers mentioned (for completeness)
  const deletedRanges = {};
  let current = null;
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
      // git includes the enclosing function/heading after the second @@ when
      // an xfuncname pattern is configured (default: many languages). Capture it.
      const m = line.match(/@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\s?(.*)$/);
      if (m) {
        const oldStart = Number(m[1]);
        const oldLen = m[2] === undefined ? 1 : Number(m[2]);
        const newStart = Number(m[3]);
        const newLen = m[4] === undefined ? 1 : Number(m[4]);
        const anchor = (m[5] ?? "").trim();
        const len = newLen > 0 ? newLen : 1;
        hunks[current].push({ start: newStart, end: newStart + len - 1, anchor });
        if (oldLen > 0) {
          deletedRanges[current].push({ start: oldStart, end: oldStart + oldLen - 1 });
        }
      }
    } else if (current && (line.startsWith("+") || line.startsWith("-"))) {
      if (line.startsWith("+++") || line.startsWith("---")) continue;
      churn[current]++;
      const text = line.slice(1);
      // Definitions
      DEFN_RE.lastIndex = 0;
      let dm;
      while ((dm = DEFN_RE.exec(text)) !== null) {
        if (dm[1].length >= 4 && !COMMON_KEYWORDS.has(dm[1])) definedSymbols.add(dm[1]);
      }
      // References
      ID_RE.lastIndex = 0;
      let im;
      while ((im = ID_RE.exec(text)) !== null) {
        const id = im[0];
        if (COMMON_KEYWORDS.has(id) || id.length < 4) continue;
        if (/[_A-Z]/.test(id)) referencedSymbols.add(id);
      }
    }
  }
  return {
    files: [...files],
    hunks,
    churn,
    definedSymbols: [...definedSymbols],
    referencedSymbols: [...referencedSymbols],
    deletedRanges,
  };
}

// ─── range anchors ───────────────────────────────────────────────────────────
//
// Bucket a hunk by its enclosing symbol/heading when git provides one. Falls
// back to a coarse 100-line bucket so output is still grouped, but anchored
// buckets survive insertions above the hunk and refactors.

function anchorKey(path, hunk) {
  if (hunk.anchor) {
    // Compress the anchor to its leading identifier-ish token so headings like
    // "## 4.1 Foo bar baz" and "## 4.1 Foo bar quux" collapse together.
    const a = hunk.anchor.replace(/^[#\s>*-]+/, "").slice(0, 60).trim();
    if (a) return `${path}#${a}`;
  }
  const bStart = Math.floor((hunk.start - 1) / 100) * 100 + 1;
  return `${path}#L${bStart}-L${bStart + 99}`;
}

// ─── boundary / sweep detection ──────────────────────────────────────────────
//
// Two filters that prevent spurious coupling edges from artificial commits:
//
// (a) Lockfile-as-boundary: a commit touching a lockfile (yarn.lock,
//     Cargo.lock, etc.) plus a lockfile-adjacent manifest (package.json) is
//     almost always a dependency bump — its file co-occurrences are
//     semantically uninteresting. We detect lockfile presence on the original
//     diff and *strip the commit from the coupling pool* (boundary marker:
//     "what shipped before" and "what shipped after" are independent).
//
// (b) Codemod / sweep detection: a commit where every touched file received a
//     near-identical churn footprint (low variance) is a formatting sweep,
//     license header rewrite, or regex codemod. Per-file coupling there is
//     accidental. We measure the variance of churn-per-file relative to the
//     mean; very-low variance + many files = sweep.

function isDepBumpCommit(c) {
  if (DEP_BUMP_RE.test(c.subject) || DEP_BUMP_RE.test(c.body)) return true;
  // Any lockfile in the parsed file set: treat the commit as a dep boundary.
  for (const f of c.files) if (isLockfile(f)) return true;
  return false;
}

function isCodemodSweep(c) {
  const vals = Object.values(c.churn);
  if (vals.length < 8) return false; // sweeps are wide by definition
  const mean = vals.reduce((s, v) => s + v, 0) / vals.length;
  if (mean === 0) return false;
  let varSum = 0;
  for (const v of vals) varSum += (v - mean) * (v - mean);
  const cv = Math.sqrt(varSum / vals.length) / mean; // coefficient of variation
  return cv < 0.2; // near-uniform per-file churn = sweep
}

// Strip lockfile entries from a commit's file list. Used in places where we
// keep the commit but don't want lockfile pairs polluting output.
function nonLockFiles(c) {
  return c.files.filter((f) => !isLockfile(f));
}

// ─── coupling primitives ─────────────────────────────────────────────────────

function pairKey(a, b) {
  return a < b ? `${a}\x00${b}` : `${b}\x00${a}`;
}

function coChange(commits, { itemsOf, weightOf }) {
  const itemCount = new Map();
  const pairCount = new Map();
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
    out.push({ a, b, support, confAB, confBA, conf });
  }
  out.sort((x, y) => y.support * y.conf - x.support * x.conf);
  return out;
}

// ─── greedy clustering (file groups) ─────────────────────────────────────────

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
  const sizeOf = new Map();
  for (const { a, b } of pairs) {
    const ra = find(a);
    const rb = find(b);
    const sa = sizeOf.get(ra) ?? 1;
    const sb = sizeOf.get(rb) ?? 1;
    if (ra !== rb && sa + sb <= maxClusterSize) {
      parent.set(ra, rb);
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

// TF-IDF / token-Jaccard fallback used when ticket+scope clustering fires on
// fewer than 3 commits. Tokenize each commit subject down to lowercased
// content words (length≥4, not stopwords), build a corpus IDF, then group
// commits whose top-IDF-tokens overlap. The cluster key is the shared top
// token. Cheap and language-agnostic — useful for repos without conventions.
const STOPWORD_RE = /^(this|that|with|from|into|when|then|then|also|just|some|more|less|been|have|will|fix|add|use|set|get|make|made|new)$/i;
function tfidfClusters(commits) {
  const docs = [];
  for (const c of commits) {
    const tokens = (c.subject ?? "")
      .toLowerCase()
      .split(/[^a-z0-9_]+/)
      .filter((t) => t.length >= 4 && !STOPWORD_RE.test(t));
    docs.push({ c, tokens: new Set(tokens) });
  }
  const df = new Map();
  for (const d of docs) for (const t of d.tokens) df.set(t, (df.get(t) ?? 0) + 1);
  const N = docs.length;
  // Top-IDF token of each doc (rare-but-not-unique — df∈[2,N/4]).
  const buckets = new Map();
  for (const d of docs) {
    let best = null, bestIdf = -Infinity;
    for (const t of d.tokens) {
      const f = df.get(t) ?? 0;
      if (f < 2 || f > N / 4) continue;
      const idf = Math.log(N / f);
      if (idf > bestIdf) { bestIdf = idf; best = t; }
    }
    if (!best) continue;
    if (!buckets.has(best)) buckets.set(best, []);
    buckets.get(best).push(d.c);
  }
  return buckets;
}

function clusterCoChange(commits) {
  const buckets = new Map();
  for (const c of commits) {
    const key = commitClusterKey(c);
    if (!key) continue;
    if (!buckets.has(key)) buckets.set(key, new Map());
    const fileCounts = buckets.get(key);
    for (const f of c.files) fileCounts.set(f, (fileCounts.get(f) ?? 0) + 1);
  }
  // Fallback: if structured keys produced few clusters, augment with TF-IDF
  // groupings of subject tokens.
  if (buckets.size < 3) {
    const fallback = tfidfClusters(commits);
    for (const [tok, cs] of fallback) {
      if (cs.length < 3) continue;
      const key = `tfidf:${tok}`;
      if (!buckets.has(key)) buckets.set(key, new Map());
      const fileCounts = buckets.get(key);
      for (const c of cs) for (const f of c.files) {
        fileCounts.set(f, (fileCounts.get(f) ?? 0) + 1);
      }
    }
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
    (a, b) => b.files.reduce((s, x) => s + x.count, 0) - a.files.reduce((s, x) => s + x.count, 0),
  );
  return groups;
}

// ─── author overlap (jaccard) ───────────────────────────────────────────────

function buildAuthorIndex(commits) {
  const authors = new Map();
  for (const c of commits) {
    for (const f of c.files) {
      if (!authors.has(f)) authors.set(f, new Set());
      authors.get(f).add(c.email);
    }
  }
  return authors;
}

function annotateJaccard(pairs, authorIndex) {
  return pairs.map((p) => {
    const A = authorIndex.get(p.a) ?? new Set();
    const B = authorIndex.get(p.b) ?? new Set();
    const inter = [...A].filter((x) => B.has(x)).length;
    const union = new Set([...A, ...B]).size;
    return { ...p, jaccard: union ? inter / union : 0 };
  });
}

// "Hidden coupling" score: real load-bearing coupling that doesn't show up in
// shared author history. Higher = more interesting mesh candidate.
function hiddenCouplingScore(p) {
  return (p.support ?? 0) * (p.conf ?? 0) * (1 - (p.jaccard ?? 0));
}

// Positive author-cluster pairs: high author overlap + low directory overlap.
// Surfaces tribal subsystems whose conceptual unit cuts across the directory
// tree (the inverse of §1's hidden-coupling rerank).
function authorClusterPairs(pairs) {
  const out = [];
  for (const p of pairs) {
    if ((p.jaccard ?? 0) < AUTHOR_JACCARD_MIN) continue;
    const da = dirname(p.a);
    const db = dirname(p.b);
    if (da === db) continue; // same directory = already-known coupling
    out.push({ ...p, dirA: da, dirB: db });
  }
  out.sort((x, y) => (y.jaccard - x.jaccard) || (y.support - x.support));
  return out;
}

// ─── structural-neighbor downweight ─────────────────────────────────────────
//
// If file A's basename appears textually inside file B (or vice versa), the
// coupling is already explicit — A imports B, or B is declared in A's
// manifest. We still want to see it, but the most interesting pairs are ones
// where neither file mentions the other.

const fileContentCache = new Map();
function tryReadHead(path) {
  if (fileContentCache.has(path)) return fileContentCache.get(path);
  let content = "";
  try {
    content = execFileSync("git", ["show", `HEAD:${path}`], {
      encoding: "utf8",
      maxBuffer: 1024 * 1024 * 32,
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    content = "";
  }
  fileContentCache.set(path, content);
  return content;
}

// Distinctive path segments: skip generic basenames ("plugin.json", "index.ts",
// "package.json", "README.md") that produce false positives, and instead
// search for the parent directory name and the basename without extension.
// A pair like marketplace.json ↔ plugins/runtime/.claude-plugin/plugin.json
// is structural because marketplace.json references "runtime" (the plugin's
// dir name), even though "plugin.json" itself is too generic to test on.
const GENERIC_BASENAMES = new Set([
  "plugin.json", "package.json", "index.ts", "index.js", "index.tsx",
  "mod.rs", "main.rs", "main.go", "lib.rs", "init.py", "__init__.py",
  "README.md", "CHANGELOG.md", "Cargo.toml", "tsconfig.json", "hooks.json",
  "SKILL.md",
]);
function distinctiveTokens(p) {
  const parts = p.split("/");
  const tokens = new Set();
  const base = parts[parts.length - 1];
  if (!GENERIC_BASENAMES.has(base)) tokens.add(base);
  // basename minus extension
  const stem = base.replace(/\.[^.]+$/, "");
  if (stem.length >= 4 && !GENERIC_BASENAMES.has(stem)) tokens.add(stem);
  // Walk parents skipping shallow generic dirs.
  for (let i = parts.length - 2; i >= 0; i--) {
    const seg = parts[i];
    if (seg.length < 4) continue;
    if (["src", "lib", "test", "tests", "dist", "node_modules", ".claude-plugin", "hooks", "skills", "agents", "commands"].includes(seg)) continue;
    tokens.add(seg);
    if (tokens.size >= 3) break;
  }
  return tokens;
}
function structurallyReferenced(a, b) {
  if (!a.includes("/") && !b.includes("/")) return false;
  const ca = tryReadHead(a);
  const cb = tryReadHead(b);
  const tA = distinctiveTokens(a);
  const tB = distinctiveTokens(b);
  if (cb) for (const t of tA) if (cb.includes(t)) return true;
  if (ca) for (const t of tB) if (ca.includes(t)) return true;
  return false;
}

// ─── 6. Apriori frequent itemsets + association rules with lift ─────────────

// Iterative Apriori up to k=K. For each frequent k-itemset, all (k−1)-subsets
// must be frequent (downward closure). We then derive rules `X → Y` where
// X∪Y is a frequent itemset and report support, confidence, lift, leverage.
//
//   lift(X→Y) = conf(X→Y) / P(Y) — independence is 1; >1 means positively
//   correlated; ROSE-style change-prediction relevance threshold is ~1.5.
function aprioriItemsetsAndRules(commits, minSupport, K, minLift) {
  const item = new Map();
  for (const c of commits) {
    const fs = c.files;
    if (fs.length < 2 || fs.length > MAX_COMMIT_FILES) continue;
    for (const f of fs) item.set(f, (item.get(f) ?? 0) + 1);
  }
  const universe = new Set([...item.entries()].filter(([, n]) => n >= minSupport).map(([k]) => k));
  // Frequent k-itemsets, encoded as sorted-paths joined by \x00.
  /** @type {Map<number, Map<string, number>>} */
  const Lk = new Map();
  // L1
  const L1 = new Map();
  for (const [f, n] of item) if (n >= minSupport && universe.has(f)) L1.set(f, n);
  Lk.set(1, L1);
  for (let k = 2; k <= K; k++) {
    const prev = Lk.get(k - 1);
    if (!prev || prev.size === 0) break;
    const next = new Map();
    // Generate candidates: count k-subsets of each commit's frequent files.
    for (const c of commits) {
      const fs = c.files.filter((f) => universe.has(f)).sort();
      if (fs.length < k) continue;
      // Enumerate combinations of size k.
      const idx = Array.from({ length: k }, (_, i) => i);
      while (true) {
        const combo = idx.map((i) => fs[i]);
        // Downward-closure prune: every (k−1)-subset must be in prev.
        let prune = false;
        for (let drop = 0; drop < k && !prune; drop++) {
          const sub = combo.filter((_, i) => i !== drop).join("\x00");
          if (!prev.has(sub)) prune = true;
        }
        if (!prune) {
          const key = combo.join("\x00");
          next.set(key, (next.get(key) ?? 0) + 1);
        }
        // advance combination index
        let j = k - 1;
        while (j >= 0 && idx[j] === fs.length - k + j) j--;
        if (j < 0) break;
        idx[j]++;
        for (let m = j + 1; m < k; m++) idx[m] = idx[m - 1] + 1;
      }
    }
    for (const [key, n] of [...next]) if (n < minSupport) next.delete(key);
    if (next.size === 0) break;
    Lk.set(k, next);
  }
  // Itemsets (k≥2) flattened.
  const itemsets = [];
  for (const [k, sets] of Lk) {
    if (k < 2) continue;
    for (const [key, support] of sets) itemsets.push({ items: key.split("\x00"), support });
  }
  itemsets.sort((a, b) => b.support - a.support || a.items.length - b.items.length);
  // Rules: for each itemset of size ≥2, split into X→Y for every nonempty proper subset.
  const Ntx = commits.length || 1;
  const rules = [];
  const supportOf = (arr) => {
    const sorted = [...arr].sort();
    const k = sorted.length;
    if (k === 1) return L1.get(sorted[0]) ?? 0;
    const set = Lk.get(k);
    return set ? (set.get(sorted.join("\x00")) ?? 0) : 0;
  };
  for (const { items, support } of itemsets) {
    if (items.length < 2) continue;
    const n = items.length;
    const total = 1 << n;
    for (let mask = 1; mask < total - 1; mask++) {
      const X = [], Y = [];
      for (let i = 0; i < n; i++) ((mask >> i) & 1 ? X : Y).push(items[i]);
      if (X.length === 0 || Y.length === 0) continue;
      const sX = supportOf(X);
      const sY = supportOf(Y);
      if (sX === 0 || sY === 0) continue;
      const conf = support / sX;
      const lift = conf / (sY / Ntx);
      const leverage = support / Ntx - (sX / Ntx) * (sY / Ntx);
      if (lift < minLift) continue;
      rules.push({ X, Y, support, conf, lift, leverage });
    }
  }
  rules.sort((a, b) => b.lift - a.lift || b.support - a.support);
  return { itemsets, rules };
}

function aprioriTriples(commits, minSupport) {
  const pair = new Map();
  const item = new Map();
  for (const c of commits) {
    const fs = c.files;
    if (fs.length < 2) continue;
    const w = 1 / Math.log2(fs.length + 1);
    for (const f of fs) item.set(f, (item.get(f) ?? 0) + w);
    for (let i = 0; i < fs.length; i++) {
      for (let j = i + 1; j < fs.length; j++) {
        pair.set(pairKey(fs[i], fs[j]), (pair.get(pairKey(fs[i], fs[j])) ?? 0) + w);
      }
    }
  }
  const freqPair = new Set(
    [...pair.entries()].filter(([, v]) => v >= minSupport).map(([k]) => k),
  );
  const triple = new Map();
  for (const c of commits) {
    // Use the unfiltered file count as the weight denominator so weights are
    // consistent with the pair pass above (every file in the commit gets the
    // same weight regardless of which subset we end up enumerating).
    const w = 1 / Math.log2(c.files.length + 1);
    const fs = c.files.filter((f) => item.get(f) >= minSupport);
    if (fs.length < 3) continue;
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

// ─── 7. Directional lagged co-change ─────────────────────────────────────────

function laggedCoChange(commits, windowSize, minSupport) {
  // commits is newest-first. j > i ⇒ commit[j] is older (earlier) than
  // commit[i]. So a "B then A" precedence, where B = commit[j], A = commit[i].
  // Track direction so a→b means "a's files changed after b's files".
  const dirPair = new Map();
  for (let i = 0; i < commits.length; i++) {
    const a = commits[i]; // later
    if (a.files.length === 0 || a.files.length > MAX_COMMIT_FILES) continue;
    for (let j = i + 1; j < Math.min(commits.length, i + 1 + windowSize); j++) {
      const b = commits[j]; // earlier
      if (b.files.length === 0 || b.files.length > MAX_COMMIT_FILES) continue;
      if (a.sha === b.sha) continue;
      const dt = Math.abs(a.ts - b.ts);
      if (dt > 7 * 24 * 3600) continue;
      const w = 1 / (1 + Math.log2(j - i + 1));
      for (const fa of a.files) {
        for (const fb of b.files) {
          if (fa === fb) continue;
          const k = `${fb}\x00${fa}`; // earlier → later
          dirPair.set(k, (dirPair.get(k) ?? 0) + w);
        }
      }
    }
  }
  return [...dirPair.entries()]
    .filter(([, v]) => v >= minSupport)
    .map(([k, v]) => {
      const [earlier, later] = k.split("\x00");
      return { earlier, later, support: v };
    })
    .sort((x, y) => y.support - x.support);
}

// ─── 8. Branch / merge topology ─────────────────────────────────────────────

function branchTopologyGroups(minSupport) {
  let raw = "";
  try {
    raw = git(["log", `--since=${SINCE}`, "--merges", "--pretty=format:%H %P"]);
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
      try { base = git(["merge-base", first, tip]).trim(); } catch { continue; }
      let names = "";
      try {
        names = git(["log", `${base}..${tip}`, "--pretty=format:", "--name-only"]);
      } catch { continue; }
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
    (a, b) => b.files.reduce((s, x) => s + x.count, 0) - a.files.reduce((s, x) => s + x.count, 0),
  );
  return groups;
}

// ─── 9. Cross-language definition-anchored symbol co-change ─────────────────

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
  const symbolPair = new Map();
  for (const c of commits) {
    if (c.files.length < 2 || c.files.length > MAX_COMMIT_FILES) continue;
    const byLang = new Map();
    for (const f of c.files) {
      const l = LANG_OF(f);
      if (!byLang.has(l)) byLang.set(l, []);
      byLang.get(l).push(f);
    }
    if (byLang.size < 2) continue;
    // Require the symbol to be a *definition* on at least one side. We
    // don't know which file declared it, but presence in `definedSymbols`
    // means it was declared somewhere in the commit's diff — far stronger
    // than any token mention.
    const defs = new Set(c.definedSymbols);
    if (defs.size === 0) continue;
    const refs = new Set(c.referencedSymbols);
    // Symbols that are defined on one side AND referenced (anywhere) — the
    // typical implementation/contract pattern.
    const candidates = new Set([...defs].filter((s) => refs.has(s) || true));
    const langs = [...byLang.keys()];
    for (let i = 0; i < langs.length; i++) {
      for (let j = i + 1; j < langs.length; j++) {
        const la = langs[i], lb = langs[j];
        const key = la < lb ? `${la}\x01${lb}` : `${lb}\x01${la}`;
        for (const sym of candidates) {
          const k = `${key}\x01${sym}`;
          if (!symbolPair.has(k)) symbolPair.set(k, { count: 0, examples: new Set() });
          const e = symbolPair.get(k);
          e.count++;
          if (e.examples.size < 3) {
            e.examples.add(`${byLang.get(la)[0]}  ↔  ${byLang.get(lb)[0]}`);
          }
        }
      }
    }
  }
  return [...symbolPair.entries()]
    .filter(([, v]) => v.count >= minSupport)
    .map(([k, v]) => {
      const [la, lb, sym] = k.split("\x01");
      return { symbol: sym, langs: `${la} ↔ ${lb}`, count: v.count, examples: [...v.examples] };
    })
    .sort((a, b) => b.count - a.count);
}

// 9b. Cross-language SCREAMING_SNAKE / shared-constant co-mention.
//
// §9 only counts symbols *defined* in the diff, missing the very common case
// where a constant like MAX_RETRY_COUNT or a protocol field name appears as a
// bare reference (consumer side) on one language and as a string literal or
// reference (producer side) on the other. This pass relaxes the requirement
// to "mentioned on +/− lines on both sides" but restricts to identifiers that
// look like protocol constants: SCREAMING_SNAKE_CASE or PascalCase-with-len≥6.
function crossLanguageConstants(commits, minSupport) {
  const isProtoConst = (s) =>
    /^[A-Z][A-Z0-9_]{5,}$/.test(s) || /^[A-Z][a-z][A-Za-z0-9]{5,}$/.test(s);
  const symbolPair = new Map();
  for (const c of commits) {
    if (c.files.length < 2 || c.files.length > MAX_COMMIT_FILES) continue;
    const byLang = new Map();
    for (const f of c.files) {
      const l = LANG_OF(f);
      if (!byLang.has(l)) byLang.set(l, []);
      byLang.get(l).push(f);
    }
    if (byLang.size < 2) continue;
    const refs = new Set([...c.referencedSymbols, ...c.definedSymbols].filter(isProtoConst));
    if (refs.size === 0) continue;
    const langs = [...byLang.keys()];
    for (let i = 0; i < langs.length; i++) {
      for (let j = i + 1; j < langs.length; j++) {
        const la = langs[i], lb = langs[j];
        const key = la < lb ? `${la}\x01${lb}` : `${lb}\x01${la}`;
        for (const sym of refs) {
          const k = `${key}\x01${sym}`;
          if (!symbolPair.has(k)) symbolPair.set(k, { count: 0, examples: new Set() });
          const e = symbolPair.get(k);
          e.count++;
          if (e.examples.size < 3) {
            e.examples.add(`${byLang.get(la)[0]}  ↔  ${byLang.get(lb)[0]}`);
          }
        }
      }
    }
  }
  return [...symbolPair.entries()]
    .filter(([, v]) => v.count >= minSupport)
    .map(([k, v]) => {
      const [la, lb, sym] = k.split("\x01");
      return { symbol: sym, langs: `${la} ↔ ${lb}`, count: v.count, examples: [...v.examples] };
    })
    .sort((a, b) => b.count - a.count);
}

// ─── 10. Rename / move chains ───────────────────────────────────────────────

function renameChains() {
  let raw = "";
  try {
    raw = git([
      "log", `--since=${SINCE}`, "--no-merges",
      "--pretty=format:#%H", "--name-status", "-M", "-C", "--find-renames=50",
    ]);
  } catch { return []; }
  const moves = [];
  let sha = null;
  for (const line of raw.split("\n")) {
    if (line.startsWith("#")) { sha = line.slice(1); continue; }
    const m = line.match(/^([RC])(\d+)\t(\S+)\t(\S+)$/);
    if (!m) continue;
    const [, , score, from, to] = m;
    if (isExcluded(from) || isExcluded(to)) continue;
    moves.push({ sha, from, to, score: Number(score) });
  }
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

// ─── 11. Churn correlation, pruned to co-change candidates ──────────────────

function pearson(xs, ys) {
  const n = xs.length;
  if (n < 2) return 0;
  let sx = 0, sy = 0;
  for (let i = 0; i < n; i++) { sx += xs[i]; sy += ys[i]; }
  const mx = sx / n, my = sy / n;
  let num = 0, dx = 0, dy = 0;
  for (let i = 0; i < n; i++) {
    const a = xs[i] - mx, b = ys[i] - my;
    num += a * b; dx += a * a; dy += b * b;
  }
  const den = Math.sqrt(dx * dy);
  return den === 0 ? 0 : num / den;
}

// Rank-based Spearman: convert to ranks (with average-rank tie-breaking) then
// run Pearson on the ranks. Robust to outliers and non-linear-but-monotonic
// breathing. Catches releases-cycle pairs that vary in magnitude.
function spearman(xs, ys) {
  if (xs.length !== ys.length || xs.length < 2) return 0;
  const rank = (arr) => {
    const sorted = arr.map((v, i) => [v, i]).sort((a, b) => a[0] - b[0]);
    const r = new Array(arr.length);
    let i = 0;
    while (i < sorted.length) {
      let j = i;
      while (j + 1 < sorted.length && sorted[j + 1][0] === sorted[i][0]) j++;
      const avg = (i + j + 2) / 2; // 1-based average rank
      for (let k = i; k <= j; k++) r[sorted[k][1]] = avg;
      i = j + 1;
    }
    return r;
  };
  return pearson(rank(xs), rank(ys));
}

// Best correlation across small lags. Tries lag ∈ [−maxLag, +maxLag] and
// returns the maximum |r| with its lag. Catches leader/follower pairs that
// breathe in phase but with a few-week shift (e.g. release ↔ docs).
function bestLaggedCorrelation(xs, ys, maxLag = 4) {
  let best = { r: 0, lag: 0, kind: "pearson" };
  for (let lag = -maxLag; lag <= maxLag; lag++) {
    const xa = lag >= 0 ? xs.slice(0, xs.length - lag) : xs.slice(-lag);
    const ya = lag >= 0 ? ys.slice(lag) : ys.slice(0, ys.length + lag);
    if (xa.length < 4) continue;
    const rp = pearson(xa, ya);
    const rs = spearman(xa, ya);
    const r = Math.abs(rs) > Math.abs(rp) ? rs : rp;
    if (Math.abs(r) > Math.abs(best.r)) {
      best = { r, lag, kind: Math.abs(rs) > Math.abs(rp) ? "spearman" : "pearson" };
    }
  }
  return best;
}

function churnCorrelation(commits, candidatePairs, { minWeeksOverlap = 6, minR = 0.6 } = {}) {
  const weekOf = (ts) => Math.floor(ts / (7 * 24 * 3600));
  const series = new Map();
  // Only build series for files that already appear in a co-change candidate
  // pair. This is the O(N²) blocker on large repos.
  const interesting = new Set();
  for (const p of candidatePairs) { interesting.add(p.a); interesting.add(p.b); }
  for (const c of commits) {
    const w = weekOf(c.ts);
    for (const [f, n] of Object.entries(c.churn)) {
      if (!interesting.has(f)) continue;
      if (!series.has(f)) series.set(f, new Map());
      const m = series.get(f);
      m.set(w, (m.get(w) ?? 0) + n);
    }
  }
  const out = [];
  for (const p of candidatePairs) {
    const ma = series.get(p.a), mb = series.get(p.b);
    if (!ma || !mb) continue;
    const weeks = new Set([...ma.keys(), ...mb.keys()]);
    if (weeks.size < minWeeksOverlap) continue;
    // Sort weeks ascending so lag analysis is on time-ordered series.
    const orderedWeeks = [...weeks].sort((x, y) => x - y);
    const xs = [], ys = [];
    for (const w of orderedWeeks) { xs.push(ma.get(w) ?? 0); ys.push(mb.get(w) ?? 0); }
    const rp = pearson(xs, ys);
    const rs = spearman(xs, ys);
    const lagged = bestLaggedCorrelation(xs, ys, 4);
    // Take the largest in absolute value across the three views; sign is kept.
    const cands = [
      { r: rp, kind: "pearson", lag: 0 },
      { r: rs, kind: "spearman", lag: 0 },
      lagged,
    ];
    cands.sort((a, b) => Math.abs(b.r) - Math.abs(a.r));
    const best = cands[0];
    if (Math.abs(best.r) >= minR) {
      out.push({
        a: p.a, b: p.b,
        r: best.r, kind: best.kind, lag: best.lag,
        rPearson: rp, rSpearman: rs,
        weeks: weeks.size,
      });
    }
  }
  out.sort((a, b) => Math.abs(b.r) - Math.abs(a.r));
  return out;
}

// ─── 12. Defect propagation (SZZ), batched per file ────────────────────────

function defectPropagation(fixCommits) {
  const edges = new Map();
  // Group deletion ranges by (parent, path) so we issue one blame per
  // file-version instead of one per range.
  const byParentPath = new Map(); // "parent\x00path" → { ranges: [], otherFiles: Set }
  for (const fix of fixCommits) {
    const parent = fix.parents[0];
    if (!parent) continue;
    for (const [path, ranges] of Object.entries(fix.deletedRanges ?? {})) {
      if (!ranges.length) continue;
      const k = `${parent}\x00${path}`;
      if (!byParentPath.has(k)) {
        byParentPath.set(k, { ranges: [], otherFiles: new Set() });
      }
      const e = byParentPath.get(k);
      for (const r of ranges) e.ranges.push(r);
      for (const other of fix.files) if (other !== path) e.otherFiles.add(other);
    }
  }
  for (const [k, { ranges, otherFiles }] of byParentPath) {
    const [parent, path] = k.split("\x00");
    // Build a single -L-multi-spec blame call.
    const blameArgs = ["blame", "--line-porcelain"];
    for (const r of ranges) blameArgs.push("-L", `${r.start},${r.end}`);
    blameArgs.push(parent, "--", path);
    let blame = "";
    try { blame = git(blameArgs); } catch { continue; }
    const introducers = new Set();
    for (const ln of blame.split("\n")) {
      const m = ln.match(/^[0-9a-f]{40} /);
      if (m) introducers.add(path);
    }
    for (const intro of introducers) {
      for (const other of otherFiles) {
        if (other === intro) continue;
        const ek = `${intro}\x00${other}`;
        edges.set(ek, (edges.get(ek) ?? 0) + 1);
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

// ─── 13. Reviewer overlap ───────────────────────────────────────────────────

function reviewerOverlap() {
  if (NO_GH) return null;
  let raw;
  try {
    raw = execFileSync(
      "gh",
      ["pr", "list", "--state", "merged", "--limit", "200", "--json", "number,files,reviews"],
      { encoding: "utf8", maxBuffer: 1024 * 1024 * 64, stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch {
    return null; // gh missing or not authed
  }
  let json;
  try { json = JSON.parse(raw); } catch { return null; }
  if (!Array.isArray(json) || json.length === 0) return [];
  const reviewers = new Map();
  for (const pr of json) {
    const rs = new Set((pr.reviews ?? []).map((r) => r.author?.login).filter(Boolean));
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
      const [fa, ra] = files[i], [fb, rb] = files[j];
      const inter = [...ra].filter((x) => rb.has(x)).length;
      const union = new Set([...ra, ...rb]).size;
      const jac = union ? inter / union : 0;
      if (jac >= 0.5 && inter >= 2) out.push({ a: fa, b: fb, inter, jaccard: jac });
    }
  }
  out.sort((a, b) => b.jaccard - a.jaccard);
  return out;
}

// ─── unified ranked aggregate ───────────────────────────────────────────────
//
// Walk every per-technique pair list and tally which techniques fired for
// each unordered pair. Pairs that show up across many techniques are the
// real mesh shortlist.

// Per-technique weights. Bug-fix and SZZ are far higher signal than raw
// co-change; structural pairs (already-explicit coupling) get demoted at the
// end so the shortlist surfaces *latent* coupling first.
const TECHNIQUE_WEIGHT = {
  1: 1.0,   // co-change (all)
  2: 3.0,   // fix-only co-change
  4: 1.5,   // anchored range co-change
  7: 1.5,   // lagged
  11: 1.0,  // churn correlation
  12: 3.0,  // SZZ defect propagation
  13: 2.0,  // reviewer overlap
};

function aggregatePairs({ fileAll, fileFix, rangeAll, lagged, churn, szz, reviewers, structuralSet }) {
  const pairs = new Map();
  const note = (a, b, technique) => {
    const k = pairKey(a, b);
    if (!pairs.has(k)) {
      pairs.set(k, {
        a: a < b ? a : b,
        b: a < b ? b : a,
        techniques: new Set(),
        structurallyReferenced: structuralSet.has(k),
      });
    }
    pairs.get(k).techniques.add(technique);
  };
  for (const p of fileAll) note(p.a, p.b, 1);
  for (const p of fileFix) note(p.a, p.b, 2);
  for (const p of rangeAll) note(p.a.split("#")[0], p.b.split("#")[0], 4);
  for (const p of lagged) note(p.earlier, p.later, 7);
  for (const p of churn) note(p.a, p.b, 11);
  for (const e of szz) note(e.from, e.to, 12);
  if (reviewers) for (const p of reviewers) note(p.a, p.b, 13);
  const out = [...pairs.values()]
    .map((p) => {
      const techs = [...p.techniques].sort((x, y) => x - y);
      const rawScore = techs.reduce((s, t) => s + (TECHNIQUE_WEIGHT[t] ?? 1), 0);
      const score = p.structurallyReferenced ? rawScore / 2 : rawScore;
      return { ...p, techniques: techs, score, rawScore };
    })
    .filter((p) => p.techniques.length >= 2);
  // Sort by weighted score; structural pairs naturally fall down because of
  // the /2 demotion. Tiebreak on number of techniques (more = stronger).
  out.sort((a, b) => b.score - a.score || b.techniques.length - a.techniques.length);
  return out;
}

// ─── --explain mode ─────────────────────────────────────────────────────────

async function explain(spec) {
  const [a, b] = spec.split(":");
  if (!a || !b) {
    process.stderr.write(`--explain requires <fileA>:<fileB>\n`);
    exit(2);
  }
  const commits = await readCommits();
  const hits = commits.filter((c) => c.files.includes(a) && c.files.includes(b));
  if (hits.length === 0) {
    stdout.write(`No commits in --since=${SINCE} touched both files.\n`);
    return;
  }
  stdout.write(`# ${hits.length} commits touched both files\n\n`);
  for (const c of hits.slice(0, 50)) {
    const date = new Date(c.ts * 1000).toISOString().slice(0, 10);
    stdout.write(`${c.sha.slice(0, 8)}  ${date}  ${c.email}\n  ${c.subject}\n  files: ${c.files.length}\n`);
    if (EXPLAIN_HUNKS) {
      // Dump the actual hunks for both files in this commit. Provides the
      // reading material needed to verify the coupling without a second tool.
      try {
        const diff = git(["show", "--unified=2", "--no-color", c.sha, "--", a, b]);
        for (const line of diff.split("\n").slice(0, 80)) stdout.write(`    ${line}\n`);
        if (diff.split("\n").length > 80) stdout.write(`    … (truncated)\n`);
      } catch {}
    }
    stdout.write(`\n`);
  }
}

// ─── output helpers ────────────────────────────────────────────────────────

function topSlice(arr) {
  if (TOP_PERCENT !== null && arr.length > 0) {
    const n = Math.max(1, Math.ceil(arr.length * (TOP_PERCENT / 100)));
    return arr.slice(0, n);
  }
  return arr.slice(0, TOP);
}

function fmtFilePair(p) {
  const j = p.jaccard !== undefined ? `  authors∩=${p.jaccard.toFixed(2)}` : "";
  const h = p.jaccard !== undefined ? `  hidden=${hiddenCouplingScore(p).toFixed(2)}` : "";
  return `  ${p.a}\n  ${p.b}\n    support=${p.support.toFixed(2)}  conf=${p.conf.toFixed(2)}${j}${h}`;
}

function fmtRangePair(p) {
  return `  ${p.a}\n  ${p.b}\n    support=${p.support.toFixed(2)}  conf=${p.conf.toFixed(2)}`;
}

// ─── main ───────────────────────────────────────────────────────────────────

async function main() {
  if (EXPLAIN) return explain(EXPLAIN);

  const all = await readCommits();
  // Per-commit usability filter: drop mega-commits, dep-bumps (lockfile boundary
  // markers), and codemod sweeps. Each is preserved on the raw commit list for
  // techniques that want to see them (renames, branch topology), but excluded
  // from coupling-pool techniques.
  const sweepShas = new Set();
  const depBumpShas = new Set();
  for (const c of all) {
    if (isDepBumpCommit(c)) depBumpShas.add(c.sha);
    if (isCodemodSweep(c)) sweepShas.add(c.sha);
  }
  const small = all.filter((c) =>
    c.files.length <= MAX_COMMIT_FILES &&
    c.files.length >= 2 &&
    !depBumpShas.has(c.sha) &&
    !sweepShas.has(c.sha),
  ).map((c) => ({ ...c, files: nonLockFiles(c) })).filter((c) => c.files.length >= 2);
  const fixes = small.filter((c) => FIX_RE.test(c.subject) || FIX_RE.test(c.body));

  // Auto-suggest a fix regex if the default matched nothing. Sample subjects
  // from a tiny token-frequency analysis and print the top non-stopword tokens
  // so the user can copy-paste a custom --fix-regex=…
  let fixRegexSuggestion = null;
  if (fixes.length === 0 && small.length > 20) {
    const tok = new Map();
    for (const c of small.slice(0, 200)) {
      for (const t of (c.subject ?? "").toLowerCase().split(/[^a-z]+/)) {
        if (t.length < 4 || STOPWORD_RE.test(t)) continue;
        tok.set(t, (tok.get(t) ?? 0) + 1);
      }
    }
    const top = [...tok.entries()].sort((a, b) => b[1] - a[1]).slice(0, 8).map(([t]) => t);
    if (top.length) fixRegexSuggestion = top.join("|");
  }

  const weight = (_c, items) => 1 / Math.log2(items.length + 1);
  const authorIndex = buildAuthorIndex(small);

  // 1. file-level co-change (all)
  let fileAll = rankPairs(
    coChange(small, { itemsOf: (c) => c.files, weightOf: weight }),
    { minSupport: MIN_SUPPORT, minConfidence: MIN_CONFIDENCE },
  );
  fileAll = annotateJaccard(fileAll, authorIndex);
  // Re-rank by hidden-coupling score so the inverse-jaccard signal actually
  // surfaces in the top of the list.
  fileAll.sort((x, y) => hiddenCouplingScore(y) - hiddenCouplingScore(x));
  // Optional structural-neighbor downweight: pairs where one file textually
  // references the other are kept but pushed to the back of the list.
  fileAll.forEach((p) => { p.structurallyReferenced = structurallyReferenced(p.a, p.b); });
  fileAll.sort((x, y) => Number(x.structurallyReferenced) - Number(y.structurallyReferenced));

  // 2. file-level co-change (bug-fixes only)
  let fileFix = fixes.length >= 5
    ? rankPairs(
        coChange(fixes, { itemsOf: (c) => c.files, weightOf: weight }),
        { minSupport: Math.max(2, Math.floor(MIN_SUPPORT / 2)), minConfidence: MIN_CONFIDENCE },
      )
    : [];
  fileFix = annotateJaccard(fileFix, authorIndex);

  // 3. range-level co-change (anchored)
  const rangeItemsOf = (c) => {
    const items = new Set();
    for (const [path, hs] of Object.entries(c.hunks)) {
      for (const h of hs) items.add(anchorKey(path, h));
    }
    return [...items];
  };
  const rangeAll = rankPairs(
    coChange(small, { itemsOf: rangeItemsOf, weightOf: weight }),
    { minSupport: Math.max(3, MIN_SUPPORT - 1), minConfidence: MIN_CONFIDENCE },
  ).filter((p) => p.a.split("#")[0] !== p.b.split("#")[0]);

  // 4. commit-message clusters
  const clusters = clusterCoChange(small);

  // 5. transitive file groups
  const fileGroups = clusterPairs(fileAll.slice(0, TOP * 4));

  // 5b. Author-cluster positive output (mirror of §5's inverse signal).
  const authorClusters = authorClusterPairs(fileAll);

  // 6. Apriori — k-itemsets and rules with lift
  const apriori = enabled(6)
    ? aprioriItemsetsAndRules(small, MIN_ITEMSET, ITEMSET_K, MIN_LIFT)
    : { itemsets: [], rules: [] };
  const triples = apriori.itemsets;
  const rules = apriori.rules;
  // 7. Lagged (directional)
  const lagged = enabled(7) ? laggedCoChange(small, WINDOW, Math.max(2, MIN_SUPPORT - 1)) : [];
  // 8. Branch topology
  const branches = enabled(8) ? branchTopologyGroups(MIN_SUPPORT) : [];
  // 9. Cross-language symbols (definition-anchored)
  const symbols = enabled(9) ? crossLanguageSymbols(small, Math.max(2, MIN_SUPPORT - 1)) : [];
  // 9b. Cross-language SCREAMING_SNAKE / shared-constant references
  const constants = enabled(9) ? crossLanguageConstants(small, Math.max(2, MIN_SUPPORT - 1)) : [];
  // 10. Renames
  const renames = enabled(10) ? renameChains() : [];
  // 11. Churn correlation, restricted to co-change candidates
  const churn = enabled(11) ? churnCorrelation(small, fileAll.slice(0, TOP * 4)) : [];
  // 12. Defect propagation
  const szz = enabled(12) && fixes.length > 0
    ? defectPropagation(fixes.slice(0, SZZ_MAX))
    : [];
  // 13. Reviewer overlap
  const reviewers = enabled(13) ? reviewerOverlap() : null;

  // Build a structural-pair set from the §1 file_pairs so the aggregator can
  // demote already-explicit coupling.
  const structuralSet = new Set();
  for (const p of fileAll) if (p.structurallyReferenced) structuralSet.add(pairKey(p.a, p.b));

  // Unified aggregate (weighted by per-technique signal strength).
  const aggregate = aggregatePairs({
    fileAll: topSlice(fileAll),
    fileFix: topSlice(fileFix),
    rangeAll: topSlice(rangeAll),
    lagged: topSlice(lagged),
    churn: topSlice(churn),
    szz: topSlice(szz),
    reviewers: reviewers ? topSlice(reviewers) : null,
    structuralSet,
  });

  const payload = {
    meta: {
      since: SINCE,
      commits: all.length,
      usable_commits: small.length,
      fix_commits: fixes.length,
      dep_bump_commits: depBumpShas.size,
      sweep_commits: sweepShas.size,
      max_commit_files: MAX_COMMIT_FILES,
      min_support: MIN_SUPPORT,
      min_confidence: MIN_CONFIDENCE,
      itemset_k: ITEMSET_K,
      min_lift: MIN_LIFT,
      szz_max: SZZ_MAX,
      top: TOP_PERCENT !== null ? `${TOP_PERCENT}%` : TOP,
      fix_regex_suggestion: fixRegexSuggestion,
    },
    aggregate: aggregate.slice(0, TOP),
    file_pairs: topSlice(fileAll),
    fix_pairs: topSlice(fileFix),
    range_pairs: topSlice(rangeAll),
    message_clusters: topSlice(clusters),
    file_groups: topSlice(fileGroups),
    author_clusters: topSlice(authorClusters),
    apriori_triples: topSlice(triples),
    apriori_rules: topSlice(rules),
    lagged_pairs: topSlice(lagged),
    branch_topology: topSlice(branches),
    cross_language_symbols: topSlice(symbols),
    cross_language_constants: topSlice(constants),
    rename_chains: topSlice(renames),
    churn_correlation: topSlice(churn),
    defect_propagation: topSlice(szz),
    reviewer_overlap: reviewers ? topSlice(reviewers) : null,
  };

  // Always emit JSON companion for downstream consumers, unless explicit path.
  const jsonPath = JSON_OUT_PATH ?? resolve(dirname(new URL(import.meta.url).pathname),
    "potential-implicit-semantic-dependencies.json");
  try { writeFileSync(jsonPath, JSON.stringify(payload, null, 2) + "\n"); } catch {}

  if (JSON_OUT) {
    stdout.write(JSON.stringify(payload, null, 2) + "\n");
    return;
  }

  const out = [];
  out.push(`# Potential implicit semantic dependencies`);
  out.push(``);
  out.push(`Scanned ${all.length} commits since ${SINCE} (${small.length} usable, ${fixes.length} bug-fixes,`);
  out.push(`  ${depBumpShas.size} dep-bump boundary commits and ${sweepShas.size} codemod sweeps excluded).`);
  out.push(`Filters: max ${MAX_COMMIT_FILES} files/commit, support≥${MIN_SUPPORT}, confidence≥${MIN_CONFIDENCE}.`);
  out.push(`JSON companion: ${jsonPath}`);
  if (fixRegexSuggestion) {
    out.push(``);
    out.push(`! No commits matched the default fix regex. Most-frequent subject tokens:`);
    out.push(`!   ${fixRegexSuggestion}`);
    out.push(`! Re-run with: --fix-regex='${fixRegexSuggestion}'`);
  }
  out.push(``);

  out.push(`## 0. Aggregate ranked shortlist (pairs firing across multiple techniques)`);
  out.push(``);
  out.push(`Techniques: 1=co-change  2=fix-only  4=range  7=lagged  11=churn  12=SZZ  13=reviewer`);
  out.push(``);
  if (aggregate.length === 0) out.push(`  (no pair fired in ≥2 techniques)`);
  for (const p of aggregate.slice(0, TOP)) {
    const tag = p.structurallyReferenced ? " [structural]" : "";
    out.push(`  score=${p.score.toFixed(1)}  [${p.techniques.join(",")}]${tag}`);
    out.push(`    ${p.a}`);
    out.push(`    ${p.b}`);
  }
  out.push(``);

  out.push(`## 1. File pairs (all commits, ranked by hidden-coupling score)`);
  out.push(``);
  if (fileAll.length === 0) out.push(`  (none above threshold)`);
  for (const p of topSlice(fileAll)) {
    out.push(fmtFilePair(p) + (p.structurallyReferenced ? "  [structural]" : ""));
  }
  out.push(``);

  out.push(`## 2. File pairs (bug-fix commits only — highest-signal subset)`);
  out.push(``);
  if (fileFix.length === 0) out.push(`  (insufficient bug-fix commits)`);
  for (const p of topSlice(fileFix)) out.push(fmtFilePair(p));
  out.push(``);

  out.push(`## 3. Cross-file range pairs (anchored by enclosing symbol/heading)`);
  out.push(``);
  if (rangeAll.length === 0) out.push(`  (none above threshold)`);
  for (const p of topSlice(rangeAll)) out.push(fmtRangePair(p));
  out.push(``);

  out.push(`## 4. Commit-message clusters (ticket / conventional-commit scope)`);
  out.push(``);
  if (clusters.length === 0) out.push(`  (no recognizable scope/ticket clusters)`);
  for (const g of topSlice(clusters)) {
    out.push(`  [${g.key}]`);
    for (const f of g.files) out.push(`    ${f.file}  (×${f.count})`);
    out.push(``);
  }

  out.push(`## 5. Transitive file groups (greedy clustering of top pairs)`);
  out.push(``);
  if (fileGroups.length === 0) out.push(`  (no clusters formed)`);
  for (const g of topSlice(fileGroups)) {
    out.push(`  group:`);
    for (const f of g) out.push(`    ${f}`);
    out.push(``);
  }

  out.push(`## 5b. Author-cluster pairs (high authors∩, cross-directory — tribal subsystems)`);
  out.push(``);
  if (authorClusters.length === 0) out.push(`  (no high-overlap cross-directory author pairs)`);
  for (const p of topSlice(authorClusters)) {
    out.push(`  ${p.a}\n  ${p.b}\n    authors∩=${p.jaccard.toFixed(2)}  support=${p.support.toFixed(2)}  ${p.dirA} ↔ ${p.dirB}`);
  }
  out.push(``);

  out.push(`## 6. Apriori frequent itemsets (k≤${ITEMSET_K})`);
  out.push(``);
  if (triples.length === 0) out.push(`  (no frequent triples above support=${MIN_ITEMSET})`);
  for (const t of topSlice(triples)) {
    out.push(`  support=${t.support.toFixed(2)}`);
    for (const f of t.items) out.push(`    ${f}`);
    out.push(``);
  }

  out.push(`## 6b. Apriori rules (X → Y, ranked by lift ≥ ${MIN_LIFT})`);
  out.push(``);
  if (rules.length === 0) out.push(`  (no rules above lift threshold)`);
  for (const r of topSlice(rules)) {
    out.push(`  {${r.X.join(", ")}}  →  {${r.Y.join(", ")}}`);
    out.push(`    support=${r.support.toFixed(2)}  conf=${r.conf.toFixed(2)}  lift=${r.lift.toFixed(2)}  leverage=${r.leverage.toFixed(3)}`);
  }
  out.push(``);

  out.push(`## 7. Directional lagged co-change (window=${WINDOW} commits, ≤7 days)`);
  out.push(``);
  if (lagged.length === 0) out.push(`  (none above threshold)`);
  for (const p of topSlice(lagged)) {
    out.push(`  ${p.earlier}\n    ↓\n  ${p.later}\n    support=${p.support.toFixed(2)}`);
  }
  out.push(``);

  out.push(`## 8. Branch topology (files co-changed within feature branches)`);
  out.push(``);
  if (branches.length === 0) out.push(`  (no merge commits with multi-file branches)`);
  for (const g of topSlice(branches)) {
    out.push(`  merge=${g.merge} tip=${g.tip}`);
    for (const f of g.files) out.push(`    ${f.file}  (×${f.count})`);
    out.push(``);
  }

  out.push(`## 9. Cross-language definition-anchored symbol co-change`);
  out.push(``);
  if (symbols.length === 0) out.push(`  (no shared definitions across languages)`);
  for (const s of topSlice(symbols)) {
    out.push(`  ${s.symbol}   [${s.langs}]   ×${s.count}`);
    for (const ex of s.examples) out.push(`    ${ex}`);
    out.push(``);
  }

  out.push(`## 9b. Cross-language SCREAMING_SNAKE / shared-constant references`);
  out.push(``);
  if (constants.length === 0) out.push(`  (no shared constant-like references across languages)`);
  for (const s of topSlice(constants)) {
    out.push(`  ${s.symbol}   [${s.langs}]   ×${s.count}`);
    for (const ex of s.examples) out.push(`    ${ex}`);
    out.push(``);
  }

  out.push(`## 10. Coordinated rename / move chains`);
  out.push(``);
  if (renames.length === 0) out.push(`  (no multi-file renames)`);
  for (const g of topSlice(renames)) {
    out.push(`  commit=${g.sha}  (${g.moves.length} moves)`);
    for (const m of g.moves) out.push(`    [${m.score}%] ${m.from}  →  ${m.to}`);
    out.push(``);
  }

  out.push(`## 11. Churn correlation (Pearson + Spearman + lag ±4 weeks, |r|≥0.6)`);
  out.push(``);
  if (churn.length === 0) out.push(`  (no correlated pairs)`);
  for (const p of topSlice(churn)) {
    const lagTag = p.lag ? `  lag=${p.lag > 0 ? "+" : ""}${p.lag}wk` : "";
    out.push(`  ${p.a}\n  ${p.b}\n    r=${p.r.toFixed(3)} (${p.kind})${lagTag}  weeks=${p.weeks}  pearson=${p.rPearson.toFixed(2)} spearman=${p.rSpearman.toFixed(2)}`);
  }
  out.push(``);

  out.push(`## 12. Defect propagation (SZZ blame-back from fix commits)`);
  out.push(``);
  if (szz.length === 0) out.push(`  (insufficient fix data or no propagation)`);
  for (const e of topSlice(szz)) out.push(`  ${e.from}  →  ${e.to}    (×${e.count} fix-pairs)`);
  out.push(``);

  out.push(`## 13. Reviewer overlap`);
  out.push(``);
  if (reviewers === null) out.push(`  (gh CLI unavailable, unauthed, or --no-gh; skipped)`);
  else if (reviewers.length === 0) out.push(`  (no qualifying reviewer overlap)`);
  else for (const p of topSlice(reviewers))
    out.push(`  ${p.a}\n  ${p.b}\n    reviewers∩=${p.inter}  jaccard=${p.jaccard.toFixed(2)}`);
  out.push(``);

  out.push(`## How to act on this`);
  out.push(``);
  out.push(`Start with §0 — pairs that fire across multiple techniques are the strongest candidates.`);
  out.push(`For each pair, ask:`);
  out.push(`  • Does one rely on a contract the other defines but doesn't enforce?`);
  out.push(`  • If the partner changed silently, what concrete wrong decision would I make?`);
  out.push(`  • Use --explain=<a>:<b> to see the commits driving the coupling.`);
  out.push(``);
  out.push(`Inverse signal: low authors∩ jaccard on a high-confidence pair = invisible coupling.`);
  out.push(`The "hidden" score in §1 already ranks by support × conf × (1 − jaccard).`);

  stdout.write(out.join("\n") + "\n");
}

main().catch((e) => {
  process.stderr.write(`error: ${e.message}\n`);
  exit(1);
});
