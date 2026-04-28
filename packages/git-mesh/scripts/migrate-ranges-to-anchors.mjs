#!/usr/bin/env node
/**
 * migrate-ranges-to-anchors.mjs
 *
 * One-shot migration for git-mesh repos created before the Range→Anchor
 * rename.  Converts:
 *   - refs/ranges/v1/<uuid>  →  refs/anchors/v1/<uuid>
 *   - blob header:  "anchor <sha>" → "commit <sha>",
 *                   "range <start> <end> <blob>\t<path>" → "extent ..."
 *   - mesh tree entry:  "ranges" file  →  "anchors" file  (same content)
 *
 * Idempotent: if refs/ranges/v1/* is already empty, exits 0 immediately.
 * Fail-closed: reads and writes all new refs/blobs before deleting any old ones.
 *
 * Usage:
 *   node migrate-ranges-to-anchors.mjs [--repo <path>] [--dry-run]
 */

import { execFileSync } from 'node:child_process';
import { parseArgs } from 'node:util';
import process from 'node:process';

const { values: flags } = parseArgs({
  options: {
    repo: { type: 'string', default: process.cwd() },
    'dry-run': { type: 'boolean', default: false },
  },
});

const repo = flags['repo'];
const dryRun = flags['dry-run'];

/** Run a git command in the target repo. Returns stdout as a utf8 string. */
function git(args, { input } = {}) {
  try {
    const result = execFileSync('git', ['-C', repo, ...args], {
      input,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    return result;
  } catch (err) {
    const msg = err.stderr ? (typeof err.stderr === 'string' ? err.stderr.trim() : err.stderr.toString().trim()) : String(err);
    throw new Error(`git ${args[0]}: ${msg}`);
  }
}

/** Run a git command and return raw stdout as a Buffer (for binary blob data). */
function gitBuf(args, { input } = {}) {
  try {
    return execFileSync('git', ['-C', repo, ...args], {
      input,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch (err) {
    const msg = err.stderr ? err.stderr.toString().trim() : String(err);
    throw new Error(`git ${args[0]}: ${msg}`);
  }
}

function log(msg) {
  process.stdout.write(msg + '\n');
}

function die(msg) {
  process.stderr.write('error: ' + msg + '\n');
  process.exit(1);
}

// ── 1. Enumerate refs/ranges/v1/* ──────────────────────────────────────────

const rangeRefsRaw = git(['for-each-ref', '--format=%(refname)', 'refs/ranges/v1']).trim();
const rangeRefs = rangeRefsRaw ? rangeRefsRaw.split('\n') : [];

if (rangeRefs.length === 0) {
  log('already migrated, no changes');
  process.exit(0);
}

// Extract uuid from "refs/ranges/v1/<uuid>"
function uuidFromRef(ref) {
  const prefix = 'refs/ranges/v1/';
  if (!ref.startsWith(prefix)) die(`unexpected ref: ${ref}`);
  return ref.slice(prefix.length);
}

// ── 2. Read + rewrite each anchor blob ────────────────────────────────────

/**
 * Rewrite old blob header bytes to new format.
 *
 * Old:  "anchor <sha>\n"  and  "range <start> <end> <blob>\t<path>\n"
 * New:  "commit <sha>\n"  and  "extent <start> <end> <blob>\t<path>\n"
 *
 * Every other byte is passed through unchanged (including "created" lines).
 * We work on a line-by-line basis over the raw Buffer so non-UTF-8 bytes
 * in the path survive unmodified.
 */
function rewriteBlob(blobBuf) {
  // Split on newline bytes while preserving the delimiters.
  const lines = [];
  let start = 0;
  for (let i = 0; i < blobBuf.length; i++) {
    if (blobBuf[i] === 0x0a /* '\n' */) {
      lines.push(blobBuf.slice(start, i + 1));
      start = i + 1;
    }
  }
  if (start < blobBuf.length) {
    lines.push(blobBuf.slice(start));
  }

  let foundCommit = false;
  let foundExtent = false;

  const rewritten = lines.map((lineBuf) => {
    const lineStr = lineBuf.toString('utf8');
    if (lineStr.startsWith('anchor ')) {
      if (foundCommit) die('duplicate `anchor` header in old blob — cannot migrate');
      foundCommit = true;
      // Replace leading "anchor " with "commit "
      return Buffer.concat([Buffer.from('commit '), lineBuf.slice(7)]);
    }
    if (lineStr.startsWith('range ')) {
      if (foundExtent) die('duplicate `range` line in old blob — cannot migrate');
      foundExtent = true;
      // Replace leading "range " with "extent "
      return Buffer.concat([Buffer.from('extent '), lineBuf.slice(6)]);
    }
    return lineBuf;
  });

  if (!foundCommit) die('old blob missing required `anchor` header — cannot migrate');
  if (!foundExtent) die('old blob missing required `range` line — cannot migrate');

  return Buffer.concat(rewritten);
}

// Map from old SHA → new SHA, and uuid → new SHA (for verification).
const uuidToNewSha = new Map();

for (const ref of rangeRefs) {
  const uuid = uuidFromRef(ref);
  // Resolve the ref to its blob sha
  const blobSha = git(['rev-parse', ref]).trim();
  // Read blob bytes
  const blobBuf = gitBuf(['cat-file', 'blob', blobSha]);
  // Rewrite
  const newBuf = rewriteBlob(blobBuf);

  if (dryRun) {
    log(`[dry-run] would rewrite blob for refs/ranges/v1/${uuid} → refs/anchors/v1/${uuid}`);
    continue;
  }

  // Write new blob
  const newSha = gitBuf(['hash-object', '-w', '--stdin'], { input: newBuf }).toString('utf8').trim();
  uuidToNewSha.set(uuid, newSha);
}

// ── 3. Create refs/anchors/v1/<uuid> ──────────────────────────────────────

if (!dryRun) {
  for (const [uuid, newSha] of uuidToNewSha) {
    git(['update-ref', `refs/anchors/v1/${uuid}`, newSha]);
  }

  // Verify every new ref resolves correctly.
  for (const [uuid, expectedSha] of uuidToNewSha) {
    const resolved = git(['rev-parse', `refs/anchors/v1/${uuid}`]).trim();
    if (resolved !== expectedSha) {
      die(`verification failed for refs/anchors/v1/${uuid}: expected ${expectedSha}, got ${resolved}`);
    }
  }
}

// ── 4. Rewrite mesh trees that reference a "ranges" file ──────────────────

const meshRefsRaw = git(['for-each-ref', '--format=%(refname)', 'refs/meshes/v1']).trim();
const meshRefs = meshRefsRaw ? meshRefsRaw.split('\n') : [];

let meshesRewritten = 0;

for (const meshRef of meshRefs) {
  if (!meshRef) continue;

  // Get the commit SHA the mesh ref points at.
  const meshCommitSha = git(['rev-parse', meshRef]).trim();

  // List top-level tree entries of the mesh commit.
  const treeEntries = git(['ls-tree', meshCommitSha]).trim();
  if (!treeEntries) continue;

  const lines = treeEntries.split('\n');
  let hasRangesFile = false;
  let hasAnchorsFile = false;
  const newEntries = [];

  for (const entry of lines) {
    // Format: "<mode> <type> <sha>\t<name>"
    const tab = entry.indexOf('\t');
    if (tab === -1) continue;
    const name = entry.slice(tab + 1);
    if (name === 'ranges') {
      hasRangesFile = true;
      // Replace filename "ranges" with "anchors" (same blob sha)
      const parts = entry.slice(0, tab).split(' ');
      // parts: [mode, type, sha]
      const [mode, , blobSha] = parts;
      newEntries.push(`${mode} blob ${blobSha}\tanchors`);
    } else if (name === 'anchors') {
      hasAnchorsFile = true;
      newEntries.push(entry);
    } else {
      newEntries.push(entry);
    }
  }

  if (!hasRangesFile) {
    // No "ranges" file — already migrated or doesn't need it.
    continue;
  }

  if (hasAnchorsFile) {
    // Both exist — unexpected; fail closed.
    die(`mesh ${meshRef} has both "ranges" and "anchors" tree entries — cannot migrate automatically`);
  }

  if (dryRun) {
    log(`[dry-run] would rewrite mesh tree for ${meshRef} (rename "ranges" → "anchors")`);
    meshesRewritten++;
    continue;
  }

  // Write the new tree.
  const mktreeInput = newEntries.join('\n') + '\n';
  const newTreeSha = git(['mktree'], { input: Buffer.from(mktreeInput) }).trim();

  // Get the original commit's metadata so we can re-commit with the new tree.
  const commitMessage = git(['log', '-1', '--format=%B', meshCommitSha]).trimEnd();
  const authorName = git(['log', '-1', '--format=%an', meshCommitSha]).trim();
  const authorEmail = git(['log', '-1', '--format=%ae', meshCommitSha]).trim();
  const authorDate = git(['log', '-1', '--format=%ai', meshCommitSha]).trim();
  const committerName = git(['log', '-1', '--format=%cn', meshCommitSha]).trim();
  const committerEmail = git(['log', '-1', '--format=%ce', meshCommitSha]).trim();
  const committerDate = git(['log', '-1', '--format=%ci', meshCommitSha]).trim();

  // Get parent commits (there should be one for normal mesh commits).
  const parentsRaw = git(['log', '-1', '--format=%P', meshCommitSha]).trim();
  const parents = parentsRaw ? parentsRaw.split(' ').filter(Boolean) : [];

  const parentArgs = parents.flatMap((p) => ['-p', p]);

  // Commit the new tree, preserving authorship and dates.
  const env = {
    ...process.env,
    GIT_AUTHOR_NAME: authorName,
    GIT_AUTHOR_EMAIL: authorEmail,
    GIT_AUTHOR_DATE: authorDate,
    GIT_COMMITTER_NAME: committerName,
    GIT_COMMITTER_EMAIL: committerEmail,
    GIT_COMMITTER_DATE: committerDate,
  };

  let newCommitSha;
  try {
    const result = execFileSync(
      'git',
      ['-C', repo, 'commit-tree', newTreeSha, ...parentArgs, '-m', commitMessage],
      { input: undefined, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'], env },
    );
    newCommitSha = result.trim();
  } catch (err) {
    const msg = err.stderr ? err.stderr.toString().trim() : String(err);
    die(`git commit-tree for ${meshRef}: ${msg}`);
  }

  git(['update-ref', meshRef, newCommitSha]);
  meshesRewritten++;
}

// ── 5. Delete old refs ─────────────────────────────────────────────────────

if (!dryRun) {
  for (const ref of rangeRefs) {
    git(['update-ref', '-d', ref]);
  }
}

// ── 6. Summary ────────────────────────────────────────────────────────────

const n = rangeRefs.length;
const m = meshesRewritten;
if (dryRun) {
  log(`[dry-run] would migrate ${n} anchor${n !== 1 ? 's' : ''} across ${m} mesh${m !== 1 ? 'es' : ''}`);
} else {
  log(`migrated ${n} anchor${n !== 1 ? 's' : ''} across ${m} mesh${m !== 1 ? 'es' : ''}`);
}
