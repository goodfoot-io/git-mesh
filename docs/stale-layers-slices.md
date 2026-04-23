# Slicing plan for stale-layers Phase 1

Addendum to `docs/stale-layers-plan.md`. Phase 1 in the main plan was
written as one shipping unit; in practice it is being delivered as a
sequence of smaller slices. This doc is the map between the two so
nothing slips through the cracks.

The Phase 1 acceptance bar in the main plan is still authoritative.
A slice is "done" when its scope is implemented *and* the Phase 1 test
matrix entries it claims to cover are passing. A slice must not claim
coverage for a test it cannot run end-to-end.

`docs/gix-filter-audit.md` documents what slice 1 wired up and what it
deferred. Keep that audit current as later slices land; each slice
that changes reader dispatch or sidecar handling owes an update there.

## Standing rules across slices

These hold for every slice, not just the ones that introduce them:

- **Fail loud in intermediate states.** Any reader path not yet
  implemented returns `RangeStatus::ContentUnavailable(FilterFailed
  { filter: "<name>" })` rather than silently falling through to
  `gix_filter` defaults. This closes footguns during the slicing
  window — a repo with a custom filter or LFS content must produce a
  clear terminal finding, not a wrong answer.
- **Sidecar acknowledgment requires the freshness stamp.** No slice
  may ship acknowledgment matching that trusts raw sidecar bytes.
  Either the sidecar freshness stamp is in place and re-normalizes on
  mismatch, or acknowledgment matching is not yet enabled.
- **Concurrency guard before multi-layer reads.** No slice that exposes
  the index or worktree layer ships without the index-file SHA-1
  trailer guard. The guard does not need to be its own slice, but it
  must land no later than the first slice that reads two layers.
- **Each slice ends green under `yarn validate`** per the
  `<golden-rule>` in `CLAUDE.md`.

## Slices

### Slice 1 — HEAD-only plumbing *(shipped)*

**Scope.** `ContentRef` type and `ContentRef::Blob` reader via
`gix::Repository::find_object`. `RangeExtent`, `RangeStatus`
(including terminal variants), `Finding`, `PendingFinding`,
`LayerSet`, `Scope` declared. CLI flags `--no-worktree`,
`--no-index`, `--no-staged-mesh`, `--ignore-unavailable` accepted.
HEAD-only fast path (`LayerSet::committed_only()`) runs end-to-end.

**Phase 1 tests covered.**

- HEAD-only mode: byte-identical output on the existing fixture.

**Deferred to later slices.** All other Phase 1 test matrix entries.

**Open debt from the gix-filter audit.** Byte-identical fixture test
against `git cat-file --filters` for the core filter set was not
written. The audit *claims* coverage; the test *proves* it. See
slice 2.

### Slice 2 — Worktree reader + Phase 0 audit closure

**Scope.**

- `ContentRef::WorktreeFile` reader via
  `gix::filter::Pipeline::convert_to_git`, with a core-filter
  allowlist: `core.autocrlf`, `core.eol`, `text`/`text=auto`,
  per-path `eol=…`, `ident`, working-tree-encoding. Any path whose
  attributes resolve to `filter=<name>` outside the allowlist
  short-circuits to `ContentUnavailable(FilterFailed { filter })`.
  No silent `gix_filter` pass-through.
- Symlink short-circuit (read link target string, no filter).
- **Byte-identical fixture test** against `git cat-file --filters` /
  `git diff` output for the full core-filter set. This closes the
  Phase 0 acceptance gap the audit flagged as honest debt.

**Phase 1 tests covered.**

- CRLF checkout of an LF blob → no false drift.
- Whole-file pin on a symlink: retarget → Changed.

**Deferred.** LFS and custom-filter reads remain `FilterFailed`
terminals until slices 6 and 7.

### Slice 3 — Index layer + acknowledgment scaffolding

**Scope.**

- `git diff-index --cached -U0 -M HEAD` parsing (unscoped, client
  filtered against `file_index`).
- `StagedIndexEntry` population; `PendingState.index` excludes
  conflicted paths (engine surfaces `MergeConflict`).
- Index-layer blob reads via `ContentRef::Blob`.
- Hunk composition for index-vs-HEAD shifts (`compute_new_range`
  parameterized for synthetic index commits).
- Rename-budget cap (1000 paths; `--no-renames` fallback with note).
- Index-file SHA-1 trailer concurrency guard (this slice or earlier —
  whichever reaches the index first).
- `Finding.source` / `Finding.current` population per the table in
  the main plan.
- Acknowledgment *scaffolding*: `acknowledged_by` field threaded
  through the types, matching by `range_id`. Actual matching stays
  disabled (returns `None`) until slice 5 ships the freshness stamp.

**Phase 1 tests covered.**

- `git add` moves drift from Worktree to Index (Worktree half stubs as
  `FilterFailed`; acceptance validates the Index half).
- `git add -p` partial staging: index half only — range shifts with
  index hunks.
- Merge-conflict path → `MergeConflict`, `current.blob = None`.
- Index-file SHA-1 trailer changes mid-run: stderr warning.
- Rename-heavy changeset (>1000 paths): no pairing blow-up; note
  rendered.

### Slice 4 — Worktree layer + full add-p composition

**Scope.**

- `git diff-files -U0 -M` parsing, client filtered.
- Worktree-vs-index hunk composition on top of slice 3's index
  shifts.
- End-to-end `--no-worktree`-off path.
- `blame_culprit` rework (blame against the commit that produced
  `current.blob`, not HEAD; only when `source == Some(Head)` and
  `current.blob.is_some()`).

**Phase 1 tests covered.**

- Worktree-only drift → `Changed`, `source=Worktree`.
- `git add` moves drift from Worktree to Index (end-to-end).
- `git add -p` partial staging: range straddles partial edit;
  both layers show drift with shifted locations.
- `git mv` across a pinned file: `Moved` with new path; mesh record's
  anchored path unchanged.
- `intent-to-add` path with a pinned range.

### Slice 5 — Staged-mesh layer + acknowledgment + sidecar stamp

**Scope.**

- `PendingFinding` population from `.git/mesh/staging/` for `Add`,
  `Remove`, `Message`, `ConfigChange` variants.
- **Sidecar freshness stamp.** Each sidecar records the active
  `.gitattributes` SHA-1 plus a hash of the filter-driver list at
  capture time. On read, engine re-normalizes both sides if the stamp
  is older than the current stamp rather than trusting stored bytes.
- Enable acknowledgment matching by `range_id`. Whole-file compares
  blob bytes; line-range compares sliced lines after re-normalization.
- `PendingFinding::{Add, Remove}` gains `drift: Option<PendingDrift>`
  populated by comparing sidecar (re-normalized) against claimed blob.
- Whole-file / line-range rejection at `git mesh add` for binary,
  symlink (line-range form), and inside-submodule paths. Submodule
  gitlink whole-file pins allowed.
- Dedup of staged adds by `(path, extent)` last-write-wins; delete
  `Error::DuplicateRangeLocation` from `mesh/commit.rs`.

**Phase 1 tests covered.**

- `git mesh add` matching sidecar → `acknowledged_by` populated,
  exit 0.
- Subsequent worktree edit invalidates the ack → exit 1.
- Ack matching survives `Moved`.
- Sidecar captured before a `.gitattributes` EOL change: re-normalized
  on read still acknowledges.
- Whole-file pin on a binary asset.
- Whole-file pin on a submodule gitlink.

### Slice 6 — LFS first-class reader

**Scope.**

- Managed `git-lfs filter-process` subprocess per `stale` run,
  spawned lazily, reused across reads, torn down at exit.
  `GIT_LFS_SKIP_SMUDGE=1` in environment (no auto-fetch).
- Pointer-OID fast path (blob OID equal across layers → `Fresh`).
- Pointer-changed-both-cached path runs the full comparator on
  smudged bytes.
- Pointer-changed-one-missing path emits
  `ContentUnavailable(LfsNotFetched)`.
- `git-lfs` binary absent emits `ContentUnavailable(LfsNotInstalled)`.
- `git mesh add` on `filter=lfs` path: require local cache; fail with
  `LfsNotFetched`-shaped error otherwise.

**Phase 1 tests covered.**

- LFS text file, content cached.
- LFS text file, content missing → `ContentUnavailable(LfsNotFetched)`;
  `--ignore-unavailable` → exit 0.
- LFS repo with no `git-lfs` binary on PATH.

### Slice 7 — Custom filter-process reader

**Scope.**

- Managed `git filter-process`-protocol subprocess per
  `filter.<name>.process` driver; lazy and reused.
- Remove the slice-2 allowlist once every driver goes through an
  explicit reader (core filters via gix, LFS via slice 6, everything
  else via slice 7). Paths whose driver isn't configured stay on
  `FilterFailed`.
- Custom-filter broken smudge → `ContentUnavailable(FilterFailed)`.

**Phase 1 tests covered.**

- Custom `filter=<name>` driver with broken smudge.

### Slice 8 — Output, performance gate, final test matrix

**Scope.**

- `cli/stale_output.rs` rewritten around `Finding` / `PendingFinding`.
  Human renderer adds the `src` column and `ack` marker. JSON /
  porcelain / JUnit / github-actions renderers under
  `{ "schema_version": 1, ... }` envelope. Snapshot tests for each
  renderer × each layer combination.
- `criterion` benchmark: HEAD-only `stale` on the existing fixture,
  measured before slice 1 and after slice 8. Regression >10% fails.
- Any remaining Phase 1 test matrix entries not covered above.

This slice is the point at which the plan's Phase 1 acceptance bar is
fully met. Slices 2–7 hold intermediate passing states; slice 8
declares Phase 1 done.

## Coverage check

Every Phase 1 test from the main plan maps to exactly one slice:

| Test | Slice |
|---|---|
| HEAD-only mode byte-identical | 1 |
| Worktree-only drift | 4 |
| `git add` moves Worktree → Index | 3 (index half), 4 (full) |
| `git mesh add` matching sidecar acknowledges | 5 |
| Ack invalidated by subsequent edit | 5 |
| Ack survives `Moved` | 5 |
| Sidecar captured before gitattributes EOL change | 5 |
| `git add -p` partial staging, both layers | 4 |
| Merge-conflict path | 3 |
| CRLF checkout of LF blob | 2 |
| Whole-file binary asset | 5 |
| Whole-file submodule gitlink | 5 |
| Whole-file symlink retarget | 2 |
| LFS text cached / missing / not installed | 6 |
| Custom filter with broken smudge | 7 |
| `git mv` rename | 4 |
| `intent-to-add` | 4 |
| Rename-heavy changeset | 3 |
| Index SHA-1 trailer mid-run | 3 |

## What this addendum does not change

- The main plan's Phase 1 acceptance bar, test matrix, and type
  definitions remain authoritative.
- Phases 2–5 in the main plan are unchanged and follow slice 8.
- Non-goals and risks in the main plan are unchanged.
