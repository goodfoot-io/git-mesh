#!/usr/bin/env node
// Explain why a specific pair of files appears coupled.
//
// Lists every commit in the window that touched both files, with subject and
// date. Use after `shortlist.mjs` surfaces a candidate pair to verify the
// coupling is real and decide whether to act on it.
//
// Usage:
//   node explain.mjs <fileA> <fileB> [--since=1.year]
//
// Equivalent to: `node mine.mjs --explain=<fileA>:<fileB> --since=…`
// but takes positional args (so the user can paste the file paths shortlist.mjs
// already printed) and skips the rest of mine.mjs's output.

import { spawn } from "node:child_process";
import { argv, exit, stdout } from "node:process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const positional = [];
const flags = {};
for (const a of argv.slice(2)) {
  if (a.startsWith("--")) {
    const [k, v] = a.replace(/^--/, "").split("=");
    flags[k] = v ?? true;
  } else {
    positional.push(a);
  }
}

if (positional.length !== 2) {
  process.stderr.write(`usage: node explain.mjs <fileA> <fileB> [--since=…] [--hunks]\n`);
  exit(2);
}
const [a, b] = positional;
const since = flags.since ?? "1.year";
const showHunks = Boolean(flags.hunks);

const here = dirname(fileURLToPath(import.meta.url));
const minePath = resolve(here, "mine.mjs");

const mineArgs = [minePath, `--explain=${a}:${b}`, `--since=${since}`];
if (showHunks) mineArgs.push("--explain-hunks");
const child = spawn(
  "node",
  mineArgs,
  { stdio: ["ignore", "pipe", "inherit"] },
);
child.stdout.pipe(stdout);
child.on("close", (code) => exit(code ?? 0));
