#!/usr/bin/env node
// Distill mine.mjs JSON output into the actionable mesh-candidate shortlist.
//
// `mine.mjs` prints 13 sections of raw signal. The most useful subset for a
// reviewer is the aggregate §0 (pairs that fire across multiple techniques).
// This script reads the JSON companion and prints just that, with optional
// per-pair drill-in showing each technique's specific signal value.
//
// Usage:
//   node shortlist.mjs [path/to/mine.json]   # default: alongside mine.mjs
//   node shortlist.mjs --top=20
//   node shortlist.mjs --min-techniques=3    # only pairs firing in ≥3 techniques

import { readFileSync } from "node:fs";
import { argv, exit, stdout } from "node:process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const args = Object.fromEntries(
  argv.slice(2).map((a) => {
    if (!a.startsWith("--")) return ["_path", a];
    const [k, v] = a.replace(/^--/, "").split("=");
    return [k, v ?? true];
  }),
);

const TOP = Number(args.top ?? 25);
const MIN_TECH = Number(args["min-techniques"] ?? 2);
const HIDE_STRUCTURAL = Boolean(args["hide-structural"]);
const MIN_SCORE = args["min-score"] !== undefined ? Number(args["min-score"]) : null;

const here = dirname(fileURLToPath(import.meta.url));
const defaultPath = resolve(here, "potential-implicit-semantic-dependencies.json");
const jsonPath = args._path ?? defaultPath;

let data;
try {
  data = JSON.parse(readFileSync(jsonPath, "utf8"));
} catch (e) {
  process.stderr.write(`error reading ${jsonPath}: ${e.message}\n`);
  process.stderr.write(`run: node mine.mjs --since=… first\n`);
  exit(1);
}

const TECHNIQUE_NAME = {
  1: "co-change",
  2: "fix-only",
  4: "range",
  7: "lagged",
  11: "churn",
  12: "SZZ",
  13: "reviewer",
};

// Build a map from pair-key → per-technique evidence so we can show why each
// pair was shortlisted, not just that it was.
function pairKey(a, b) { return a < b ? `${a}\x00${b}` : `${b}\x00${a}`; }
const evidence = new Map();
const note = (a, b, technique, detail) => {
  const k = pairKey(a, b);
  if (!evidence.has(k)) evidence.set(k, new Map());
  evidence.get(k).set(technique, detail);
};
for (const p of data.file_pairs ?? []) {
  note(p.a, p.b, 1, `support=${p.support.toFixed(1)} conf=${p.conf.toFixed(2)} jaccard=${p.jaccard?.toFixed(2) ?? "?"}`);
}
for (const p of data.fix_pairs ?? []) {
  note(p.a, p.b, 2, `support=${p.support.toFixed(1)} conf=${p.conf.toFixed(2)}`);
}
for (const p of data.range_pairs ?? []) {
  note(p.a.split("#")[0], p.b.split("#")[0], 4, `${p.a.split("#")[1]} ↔ ${p.b.split("#")[1]} support=${p.support.toFixed(1)}`);
}
for (const p of data.lagged_pairs ?? []) {
  note(p.earlier, p.later, 7, `${p.earlier} → ${p.later} support=${p.support.toFixed(1)}`);
}
for (const p of data.churn_correlation ?? []) {
  note(p.a, p.b, 11, `r=${p.r.toFixed(2)} weeks=${p.weeks}`);
}
for (const e of data.defect_propagation ?? []) {
  note(e.from, e.to, 12, `${e.from} → ${e.to} ×${e.count}`);
}
for (const p of data.reviewer_overlap ?? []) {
  note(p.a, p.b, 13, `reviewers∩=${p.inter} jaccard=${p.jaccard.toFixed(2)}`);
}

let ranked = (data.aggregate ?? []).filter((p) => p.techniques.length >= MIN_TECH);
if (HIDE_STRUCTURAL) ranked = ranked.filter((p) => !p.structurallyReferenced);
if (MIN_SCORE !== null) ranked = ranked.filter((p) => (p.score ?? 0) >= MIN_SCORE);
ranked = ranked.slice(0, TOP);

const out = [];
out.push(`# Mesh-candidate shortlist`);
out.push(``);
out.push(`Source: ${jsonPath}`);
out.push(`Window: ${data.meta?.since}, ${data.meta?.usable_commits}/${data.meta?.commits} usable commits`);
out.push(`Filter: pairs firing in ≥${MIN_TECH} techniques`);
out.push(``);
if (ranked.length === 0) {
  out.push(`No pairs met the threshold. Lower --min-techniques or widen --since in mine.mjs.`);
} else {
  for (const p of ranked) {
    const techList = p.techniques.map((t) => `${t}=${TECHNIQUE_NAME[t]}`).join(", ");
    const tag = p.structurallyReferenced ? "  [structural — coupling already explicit]" : "";
    const score = p.score !== undefined ? `score=${p.score.toFixed(1)}  ` : "";
    out.push(`## ${p.a}${tag}`);
    out.push(`   ${p.b}`);
    out.push(`   ${score}techniques (${p.techniques.length}): ${techList}`);
    const ev = evidence.get(pairKey(p.a, p.b));
    if (ev) {
      for (const t of p.techniques) {
        const d = ev.get(t);
        if (d) out.push(`     ${TECHNIQUE_NAME[t]}: ${d}`);
      }
    }
    out.push(`   drill in:  node explain.mjs "${p.a}" "${p.b}"`);
    out.push(``);
  }
}
stdout.write(out.join("\n") + "\n");
