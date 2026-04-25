# `git mesh advice` Delta Redesign Plan

## Summary

Redesign `git mesh advice` around workspace deltas and explicit read history.
This replaces the existing event-stream interface entirely. The new command
surface is:

```sh
git mesh advice <sessionId> snapshot
git mesh advice <sessionId> read <path-or-range>...
git mesh advice <sessionId>
git mesh advice <sessionId> --documentation
```

The core idea is that Git can already tell us what changed. Advice should use a
session baseline, the current workspace tree, and the last rendered tree to
derive changed paths, hunks, renames, deletions, and pre/post content. Explicit
read events remain because Git cannot infer what the developer inspected or
intended to touch.

## Goals

- Replace the pre-existing `git mesh advice <sessionId> add --...` interface.
- Make the default workflow simple enough to explain in one sentence:
  snapshot at session start, optionally record reads, then run advice whenever
  context is needed.
- Keep advice generic: no editor-specific data shapes, no network calls, no
  SQL dependency, and no hidden repository mutation.
- Preserve the DX commitments from `docs/advice-dx-goals.md`: partner-first
  routing, silence when nothing clears the bar, fail-closed heuristics, and
  repeat suppression.
- Reuse the snapshot/delta behavior described in `docs/snapshot-delta.md` as
  the baseline semantics for filesystem state.
- Treat advice as a read-only composition over existing mesh state, current
  workspace state, and session-local state.

## Design Decisions

- `snapshot` is silent on success.
- `snapshot` always resets read history, seen advice fingerprints, seen
  documentation topics, file-touch history, and `last-flush`.
- A successful bare render updates `last-flush` even when it prints nothing.
- A successful bare render records a file-touch interval when either the
  incremental workspace delta or the since-last-flush reads are non-empty.
- Session storage uses `GIT_MESH_ADVICE_DIR` when set, otherwise defaults under
  `/tmp/git-mesh`.
- Documentation-topic suppression resets on `snapshot` only.

## Non-Goals

- Do not maintain compatibility with the current `add --read`, `add --write`,
  `add --commit`, or `add --snapshot` interface.
- Do not use SQLite or an equivalent query engine for session state.
- Do not preserve staging-only Git index changes where file bytes are unchanged.
  The workspace tree model is content based. Mesh staging remains visible
  through `.git/mesh/staging`.
- Do not infer reads from shell history, editor state, prompts, or telemetry.
  Reads are only what callers explicitly record with `read`.
- Do not generate mesh descriptions. Advice may suggest `git mesh add`, but it
  should not invent a `why`.

## Command Semantics

### `git mesh advice <sessionId> snapshot`

Creates or replaces the session baseline.

Required behavior:

- Build a temporary Git tree representing the current filesystem content, using
  the same capture rules as `scripts/snapshot.sh`.
- Store the baseline tree id and the object storage needed to read it later.
- Set `last-flush` to the same tree as the baseline.
- Reset the session's read history, seen advice fingerprints, and seen
  documentation topics, and file-touch history.
- Exit zero with no stdout on success.

Rationale: replacing a baseline while retaining old read or seen state mixes two
different sessions and can suppress advice incorrectly.

### `git mesh advice <sessionId> read <path-or-range>...`

Appends one or more explicit read records to the session.

Required behavior:

- Accept the existing mesh range grammar:
  - `<path>#L<start>-L<end>`
  - `<path>` for whole-file attention
- Normalize absolute paths to repo-relative paths.
- Validate path existence and range shape. Fail closed on invalid input.
- Append records to an audit-friendly text format.
- Do not render advice and do not update `last-flush`.

Read records are durable attention context. They can trigger lightweight partner
routing even when the workspace delta is empty.

### `git mesh advice <sessionId>`

Computes current advice and renders only information not previously surfaced in
this session.

Required behavior:

- Fail clearly if no snapshot exists for this repo, worktree, and session id.
- Build the current workspace tree without mutating the checkout or real index.
- Diff `baseline -> current` for session-wide context and pre/post content.
- Diff `last-flush -> current` for newly changed content since the previous
  render.
- Load read history.
- Load current mesh state through existing mesh read/resolver APIs.
- Load mesh staging state from `.git/mesh/staging`.
- Produce advice candidates.
- Suppress candidates already recorded in the seen-fingerprint log.
- Print nothing and exit zero when no candidate remains.
- After a successful render, append emitted fingerprints, append a file-touch
  interval when the incremental delta or since-last-flush reads are non-empty,
  and update `last-flush` to the current workspace tree.

`last-flush` must advance only after successful candidate computation and output
rendering. A successful render advances `last-flush` even when no advice is
printed, because the current tree has been observed. If advice fails, the next
run should see the same delta again. The same successful render also advances
the read cursor used for future file-touch intervals.

### `git mesh advice <sessionId> --documentation`

Runs the same computation as the bare command, then appends relevant
documentation for reason kinds that appeared in this render.

Required behavior:

- Documentation is scoped to advice emitted in this invocation.
- Documentation is suppressed by a separate session-local topic seen log and
  appears at most once per session.
- If no advice candidate renders, documentation should not render by itself.
- The command still records emitted advice fingerprints and advances
  `last-flush` exactly like the bare command.
- Preserve the existing style of `--documentation` output: contextual,
  inline, comment-prefixed markdown that teaches the concept just before or
  alongside the advice it supports. The specific wording may change, but the
  output should continue to read as explanatory context inside the advice
  stream rather than as a separate manual page or generic help dump.

## Session State

Use a simple file-backed store. Suggested layout:

```text
${GIT_MESH_ADVICE_DIR:-${TMPDIR:-/tmp}/git-mesh/advice}/<repo-key>/<sessionId>/
  baseline.state
  baseline.objects/
  last-flush.state
  last-flush.objects/
  reads.jsonl
  touches.jsonl
  advice-seen.jsonl
  docs-seen.jsonl
  lock
```

`repo-key` should include both the physical worktree path and the
worktree-specific Git directory, matching `docs/snapshot-delta.md`, so linked
worktrees do not collide.

Use an advisory lock file for every command that reads or writes session state.
The state files should be written atomically through temp-file plus rename.

### State File Contents

`baseline.state` and `last-flush.state` should include:

- schema version;
- tree id;
- object directory name;
- creation time;
- repo root path;
- worktree-specific Git directory;
- read cursor for the last successful render;
- git-mesh binary/schema version if useful for fail-closed upgrades.

`reads.jsonl` records normalized attention inputs:

```json
{"ts":"2026-04-25T00:00:00Z","path":"src/api.ts","start_line":10,"end_line":20}
```

`advice-seen.jsonl` records stable fingerprints:

```json
{"ts":"2026-04-25T00:00:00Z","fingerprint":"...","reason":"partner","mesh":"billing/checkout","partner":"api/server.ts#L10-L30","trigger":"web/client.ts#L5-L12"}
```

`docs-seen.jsonl` records documentation topic keys:

```json
{"ts":"2026-04-25T00:00:00Z","topic":"editing-across-files"}
```

`touches.jsonl` records one successful-render interval whenever there was a
workspace delta or a read since the previous render:

```json
{"ts":"2026-04-25T00:00:00Z","from_tree":"abc","to_tree":"def","changed_paths":["src/api.ts"],"read_paths":["src/api.test.ts"],"paths":["src/api.ts","src/api.test.ts"]}
```

JSONL keeps the session auditable without requiring migrations, indexes, or a
database engine.

## Workspace Tree Capture

The Rust implementation should port the behavior of `scripts/snapshot.sh` and
`scripts/delta.sh` instead of shelling out to those scripts.

Required capture semantics:

- Include tracked edits.
- Include tracked deletions.
- Include untracked, non-ignored files.
- Include executable-bit mode changes.
- Preserve binary content through Git objects.
- Exclude ignored files and `.git`.
- Represent submodules as gitlinks rather than recursing.
- Do not mutate checked-out files, the real Git index, Git refs, or Git config.
- Write temporary objects into session-owned object directories, with the real
  repository object database as an alternate.

This keeps advice local and deterministic while allowing standard Git tree
diffing to produce renames, deletes, and pre/post blobs.

## Candidate Inputs

Each render should compute candidates from these inputs:

- Session-wide delta: `baseline -> current`.
- Incremental delta: `last-flush -> current`.
- Explicit read history.
- File-touch interval history.
- Current mesh ranges and whys.
- Current stale status for mesh ranges.
- Current `.git/mesh/staging` operations.
- Existing advice fingerprints and documentation-topic fingerprints.

Use session-wide delta for context-rich reasoning, such as old names, pre-edit
bytes, and whether a file has been renamed or deleted during the session. Use
incremental delta to decide what is new enough to speak about now.

Use file-touch intervals for session co-touch. Each interval represents the set
of files that moved, or were explicitly read, between two successful advice
renders. This preserves a "files moved together during this session" signal
without returning to per-edit event streaming.

## Candidate Classes

The exact list can evolve, but the first implementation should cover these
classes:

### Read Intersects Mesh

When a recorded read path or range intersects a mesh, surface the partner ranges
at low density. This replaces `add --read`.

### Delta Intersects Mesh

When a changed hunk, changed whole file, deleted path, or renamed path
intersects a mesh range, surface the partner ranges. This replaces most
`add --write` behavior.

### Partner Drift

When the current stale resolver reports a partner as `CHANGED`, `MOVED`, or a
terminal status, include the marker on the partner line. Advice should still
read as routing, not warning.

### Rename or Delete Consequence

When the session delta records a rename or deletion of a meshed path, inspect
partner ranges for stale path literals or references that can be derived from
the old name. Emit concrete commands only when the target is uniquely
determined.

### Range Shrink or Expansion

Use baseline and current blobs to compare the original meshed extent with the
post-edit extent. When a range collapses enough to change what the mesh appears
to cover, suggest the remove/add re-record sequence.

### Session Co-Touch Candidate

Use `touches.jsonl` plus historical co-change to recommend possible new meshes.
Count co-touch by interval, not by raw event count. A first version should
require:

- two paths changed together in at least two intervals, or changed/read together
  in at least three intervals;
- repeated historical co-change in Git history;
- no existing mesh covering the pair;
- no stronger enforcement mechanism detected.

Read-only intervals should be weighted lower than changed intervals. Generated,
ignored, vendored, lockfile, and huge binary paths should be filtered out before
candidate generation. If confidence is low, stay silent.

### Mesh Staging Cross-Cut

Continue to inspect `.git/mesh/staging` directly. Surface staged adds/removes
that overlap committed ranges in other meshes or would leave a mesh empty.

## Fingerprinting and Suppression

Advice suppression should be explicit and content-aware enough to avoid both
spam and stale silence.

Recommended fingerprint fields:

- reason kind;
- mesh name, if any;
- partner range address;
- trigger address or changed hunk address;
- partner status marker;
- command text, if any;
- relevant old/new path for rename/delete reasons;
- current tree id or changed blob ids when the same address can carry new
  content.

The fingerprint should not include wording-only documentation text. Copy edits
should not cause old advice to reappear unless the underlying reason changed.

## Documentation Rendering

Documentation should be keyed by reason kind and shown only through
`--documentation`.

Rules:

- Render documentation only for reason kinds emitted in the same invocation.
- Record topic keys in `docs-seen.jsonl`.
- A topic may render at most once per session. Reset recorded topic keys only
  when `snapshot` starts a new baseline.
- Keep the current inline documentation style: short conceptual paragraphs,
  concrete command examples when useful, and the same comment-prefixed markdown
  shape as the advice output. Exact legacy wording is not required.
- Keep mesh whys visible every time the mesh appears. Whys are not
  documentation topics and are not suppressed.
- Keep documentation concise and actionable. The default command without
  `--documentation` should remain readable without prior mesh knowledge.

## Failure Behavior

Fail closed:

- Missing snapshot: non-zero with a direct message to run
  `git mesh advice <sessionId> snapshot`.
- Corrupt state file: non-zero, do not silently reinitialize.
- Missing baseline objects: non-zero.
- Path/range validation failure on `read`: non-zero.
- Unable to build the current workspace tree: non-zero.
- Unable to read mesh state: non-zero unless there are truly no meshes.

No heuristic should emit advice when required inputs are unavailable.

## Migration Plan

This is a replacement, not a compatibility layer.

Implementation phases:

1. Add the new CLI surface and remove the `add` subcommand from the public
   parser.
2. Implement the file-backed session store and locking.
3. Port workspace tree capture and tree diffing into Rust.
4. Implement `snapshot`, `read`, bare render, and `--documentation`.
5. Rebuild candidate detection around delta inputs.
6. Replace plugin hook calls:
   - Session start runs `git mesh advice <sessionId> snapshot`.
   - Read-like hooks run `git mesh advice <sessionId> read <path-or-range>`.
   - Edit/stop hooks run bare `git mesh advice <sessionId>`.
7. Remove old SQL-backed advice modules and tests.
8. Update handbook command reference and reserved-name documentation.

## Test Plan

Command contract tests:

- `snapshot` creates a baseline and resets session state.
- bare advice fails clearly before `snapshot`.
- `read` accepts valid paths/ranges and rejects invalid ones.
- repeated bare advice is silent when the workspace and reads have not changed.
- successful bare advice records a touch interval when changed paths or new
  reads exist.
- successful bare advice advances the read cursor even when it prints nothing.
- `--documentation` renders only relevant docs and suppresses repeated topics.

Delta tests:

- tracked edit;
- tracked delete;
- tracked rename;
- untracked file;
- binary file;
- executable-bit change;
- ignored file exclusion;
- submodule gitlink change.

Advice behavior tests:

- read intersects mesh and surfaces partners.
- edit intersects mesh and surfaces partners.
- partner drift markers render.
- rename literal in partner renders.
- range shrink command renders only with sufficient confidence.
- staged mesh overlap renders.
- staged remove that empties a mesh renders.
- interval co-touch plus historical co-change can suggest a possible new mesh.
- same advice suppresses after render.
- advice reappears when relevant content changes again.

Regression tests:

- no repository mutation during snapshot or render;
- linked worktrees do not share session state;
- corrupt state fails closed;
- missing baseline objects fails closed.

## Open Review Questions

No open review questions are currently recorded.
