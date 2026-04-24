# Manual validation of `git-mesh`

Running the verification plan from `docs/git-mesh-the-missing-handbook.md` against
`git-mesh 0.1.0` (binary at `/home/node/.local/bin/git-mesh`) on `2026-04-23`.

Each task captures the commands run, the observed behavior, and a
PASS/FAIL/PARTIAL verdict against the handbook's claim. Scratch repos live
under `/tmp/gm-validate-*` and are created fresh per task.


## Task 1 — Preflight ✅ (with DOC BUG)

```
$ git mesh --version
git-mesh 0.1.0
$ git mesh doctor            # empty repo
INFO   MissingPostCommitHook: install at .git/hooks/post-commit with body: git mesh commit
INFO   MissingPreCommitHook:  install at .git/hooks/pre-commit  with body: git mesh pre-commit-check
WARN   FileIndexMissing — regenerated
mesh doctor: found 4 finding(s)
```

- `.git/mesh/file-index` is created lazily by `doctor` even without any mesh
  ops (exists as 16-byte file after doctor runs).
- First `git mesh add` creates `.git/mesh/staging/{m, m.1, m.1.meta}`.

**DOC BUG — `docs/git-mesh-the-missing-handbook.md:114`**: handbook's
installation snippet says the pre-commit hook should call
`git mesh pre-commit`. That subcommand does not exist; `git mesh`
interprets `pre-commit` as a mesh name and errors with
`error: mesh not found: pre-commit`. `doctor` recommends the correct
command, `git mesh pre-commit-check`. The handbook must be updated.

**MINOR DOC DRIFT — `…handbook.md:1004`**: handbook lists staging files as
`<name>`, `<name>.why`, `<name>.<N>`. Actual layout also writes
`<name>.<N>.meta` for the normalization stamp (the handbook describes the
stamp as part of `.<N>` rather than a sibling file). Not a functional
problem, just inaccurate.

## Task 2 — Hook behavior ✅

Installed both hooks (using `pre-commit-check` per doctor's recommendation).

- **Worktree-only drift**: unstaged edit to a meshed line, then commit an
  unrelated file — `git commit` succeeds. Matches handbook claim "Worktree
  drift is not a pre-commit failure" (§Installation).
- **Index-layer drift without re-anchor**: `git add` the drift, then
  `git commit` → pre-commit-check prints `I Changed … <range-id>` and
  fails with exit 1; HEAD unchanged. Matches handbook claim.
- **Index-layer drift with re-anchor staged**: `git mesh add <name> <range>`
  before commit → pre-commit-check prints `(ack)` and ADD op; commit
  proceeds; post-commit advances `refs/meshes/v1/<name>` to a new
  commit whose parent is the previous mesh tip (`git log` shows 2 entries).

Mesh commit message carried the inherited why ("covers f") across the
re-anchor without a new `git mesh why` — confirms §Updating a mesh claim.

## Task 3 — First mesh end-to-end ✅ (with minor findings)

`add` → `why` → `commit` worked. Staging layout after `add`:
```
frontend-backend-sync
frontend-backend-sync.1
frontend-backend-sync.1.meta
frontend-backend-sync.2
frontend-backend-sync.2.meta
frontend-backend-sync.why
```

`git ls-tree refs/meshes/v1/<name>` returned exactly:
```
100644 blob … config
100644 blob … ranges
```
matching `…handbook.md:982`. The `config` blob contains the resolver
settings; the `ranges` blob is a newline-separated list of range UUIDs.
Each UUID resolves to a `refs/ranges/v1/<uuid>` blob whose body carries
`anchor <commit>`, `created <timestamp>`, and `range <start> <end>
<content-sha> <path>`.

The `git mesh <name>` render shows each range as
`<anchor-commit-prefix>  <path>#L<s>-L<e>` — the 8-char prefix is the
anchor commit, not the range blob id. Worth knowing if a scripter assumes
otherwise.

`git mesh stale frontend-backend-sync` reports `0 stale of 2 ranges`
with exit 0 (FRESH). Matches handbook.

**MINOR BUG — broken-pipe panic**: `git mesh <name> --no-abbrev | head -3`
panics the Rust process with `failed printing to stdout: Broken pipe (os
error 32)` before it finishes, instead of exiting cleanly. Same shape of
issue exists throughout CLI tools that don't install a SIGPIPE handler.
Any docs example that pipes mesh output into `head`/`less` risks a
stacktrace.

## Task 4 — Range syntax rejections ✅ (with caveat)

| Attempt | Expected | Actual |
|---|---|---|
| Line-range on symlink | reject | ✅ `line-range pin rejected on symlink` |
| Line-range inside submodule | reject | ✅ `line-range pin rejected inside submodule` |
| Whole-file inside submodule | reject | ✅ `whole-file pin rejected inside submodule (only the gitlink root is allowed)` |
| Whole-file on gitlink root | accept | ✅ |
| Whole-file on binary | accept | ✅ |
| Whole-file on symlink | accept | ✅ |
| Line-range on `.gitattributes`-binary path | reject | ✅ `line-range pin rejected on binary path` |
| Line-range on NUL-byte file w/o attributes | reject *(handbook wording)* | ⚠️ **ACCEPTED** — tool uses `.gitattributes` classification only, not content sniffing |

**DOC CLARIFICATION — `…handbook.md:244`**: "Rejections at `git mesh add`:
Line-range pins on binary paths, symlinks, or paths inside a submodule."
The binary check is attribute-driven. A file with raw NUL bytes but no
`binary` attribute is accepted as a line-range pin. Either tighten the
implementation (add a content-based fallback) or tighten the doc
("binary paths (by `.gitattributes`)"). Prefer the implementation fix so
a team that forgets `.gitattributes` doesn't anchor nonsense.

## Task 5 — Layered stale `src` column ⚠️ PARTIAL (handbook example wrong)

Reproduced the handbook's partial-staging fixture exactly: 10-line
`file1.txt`, line 1 edited in the index, line 10 edited in the worktree
only. Handbook §Partial staging (lines 583–592) claims two findings with
distinct `src=I` and `src=W` rows:

```
CHANGED  m  file1.txt#L1-L10  src=I
CHANGED  m  file1.txt#L1-L10  src=W
```

Actual output:

```
$ git mesh stale m --format=porcelain
# porcelain v1
CHANGED	I	m	file1.txt	1	10	-
exit=1
```

Only a single finding is emitted — the `I` layer. The `W` layer's
additional drift is shadowed. `--no-worktree` leaves the same single `I`
row; `--no-worktree --no-index` clears all findings with exit 0. The
resolver surfaces the **shallowest** drifting layer, not one per layer.

Ack flow works: a later `git mesh add m file1.txt#L1-L10` renders
`(ack)` and exit 0 (matches handbook).

**DOC BUG — `…handbook.md:583-602`**: the "range surfaces twice" claim
and example output are incorrect. Either:
- update the handbook to describe single-row, shallowest-layer reporting
  ("the `src` column names the shallowest drifting layer"), **or**
- change the resolver to emit a finding per drifting layer (more
  informative; matches the "A finding is printed for every enabled
  layer" sentence at line 432).

The text at `…handbook.md:432` ("A finding is printed for every enabled
layer") and `…handbook.md:485-488` (exit-code contract) also reflect the
multi-row model. The whole §Understanding stale output section should be
reconciled with the implementation.

## Task 6 — Layer flag matrix ✅ (with porcelain schema quirk)

- All 8 combinations of `{--no-worktree, --no-index, --no-staged-mesh}`
  return exit 0 on a fresh mesh.
- Worktree-only drift: default exits 1 with `src=W`; `--no-worktree`
  exits 0.
- HEAD-floor: after committing drift, all three `--no-*` flags still
  exit 1 and emit a CHANGED finding — HEAD cannot be turned off.

**SCHEMA INCONSISTENCY — porcelain v1**

The handbook implies porcelain has a stable column layout. In practice the
`src` column disappears when HEAD is the only enabled layer:

```
# with any non-H layer active, 7 columns:
CHANGED	H	m	f.txt	1	10	-
CHANGED	W	m	f.txt	1	10	-
CHANGED	I	m	f.txt	1	10	-

# with --no-worktree --no-index --no-staged-mesh, 6 columns:
CHANGED	m	f.txt	1	10	-
```

Scripts keyed on column index will mis-parse. Either always emit the `src`
column (with `H` when nothing else is enabled) or document the schema
variation at `…handbook.md:511-523`.

## Task 7 — Status transitions ⚠️ PARTIAL (one CRITICAL BUG found)

| Status | Result |
|---|---|
| `FRESH` | ✅ `exit=0`, no findings. |
| `MOVED` | ✅ After inserting a line above the range: `MOVED\t-\tm\tf.txt\t5\t10\t-`, exit 1. |
| `CHANGED` | ✅ `CHANGED\tH\tm\tf.txt\t…`, exit 1. |
| `ORPHANED` | ✅ After `branch -D tmp && reflog expire && gc --prune=now`: `ORPHANED\t-\tm\tf.txt\t…`, exit 1. |
| `MERGE_CONFLICT` | ✅ modify/delete conflict → `MERGE_CONFLICT\t-\tm\tf.txt\t…`, exit 1. |
| `SUBMODULE` | ⏭️ Not engineered. Handbook notes this is a **legacy-range** status; the CLI refuses new line-range pins inside submodules, so there's no direct way to produce one. Would need a binary fixture that predates the check. |
| `CONTENT_UNAVAILABLE(LfsNotFetched)` | 🔴 Unable to test due to bug below. |
| `CONTENT_UNAVAILABLE(LfsNotInstalled)` | ⏭️ Same dependency. |
| `CONTENT_UNAVAILABLE(PromisorMissing)` | ⏭️ Would need a partial-clone setup; deferred. |
| `CONTENT_UNAVAILABLE(SparseExcluded)` | ⏭️ Deferred. |
| `CONTENT_UNAVAILABLE(FilterFailed)` | ⏭️ Deferred. |

### 🔴 CRITICAL BUG — `git mesh commit` rejects LFS line-ranges

Repro on a clean repo with `git-lfs` installed and configured:

```
git lfs install --local
echo "*.tsv filter=lfs diff=lfs merge=lfs -text" > .gitattributes
git add .gitattributes && git commit -m attr
seq 1 50 | awk '{print "row"$1 "\tval"}' > data.tsv
git add data.tsv && git commit -m data
git mesh add pn data.tsv#L1-L10    # ✅ succeeds; sidecar holds 50 real lines
git mesh commit pn                 # ❌ error: invalid range: start=1 end=10
```

The sidecar correctly captures the 50-line filtered content at `add` time
(confirmed via `wc -l .git/mesh/staging/pn.1`), but `mesh commit`
validates `L1-L10` against the **pointer blob** (3 lines), not the
filtered content. Validation path diverges from the capture path.

This contradicts the handbook's §Pinning LFS-managed files and §LFS
worked example (`…handbook.md:252-267`, `…handbook.md:642-689`) which
both rely on this flow. Whole-file LFS pins work correctly.

`CONTENT_UNAVAILABLE` reasons are documented but cannot be exercised for
line-range LFS ranges until this bug is fixed. Note also that with
whole-file LFS, wiping `.git/lfs/objects/` does **not** produce
`CONTENT_UNAVAILABLE` because whole-file compares the pointer OID —
matches §Resolver's "fast path for LFS is pointer-OID equality" at
`…handbook.md:1045`.

## Task 8 — LFS cached vs not-cached 🔴 BLOCKED

Cannot validate the handbook's "cached case" (slice diff at line 42)
because `git mesh commit` for an LFS line-range fails — see the critical
bug in Task 7. Whole-file LFS pins commit and stale cleanly but are
pointer-OID comparisons per design, so they cannot exhibit
`CONTENT_UNAVAILABLE`. The `--ignore-unavailable` flag cannot be
exercised against real LFS drift until the commit path is fixed.

## Task 9 — Whole-file pin behavior ⚠️ PARTIAL (ack bug + minor render drift)

Confirmed:
- Whole-file `add` / `why` / `commit` roundtrip works for binary, symlink,
  and submodule-gitlink targets.
- Byte swap produces `CHANGED` at the shallowest drifting layer
  (`src=W` before `git add`, `src=I` after — consistent with
  line-range behavior).
- Symlink target change → `CHANGED` (compared by target string).
- Submodule bump → `CHANGED` on the gitlink path without opening
  the submodule.

### 🐛 BUG — whole-file staged re-anchor does not produce `(ack)`

Per `…handbook.md:630-635`, a staged whole-file re-anchor should render
`(ack)` and exit 0:

```
git mesh add web assets/hero.png
git mesh stale web
# CHANGED  web  assets/hero.png  (whole)  src=I  (ack)
# exit 0
```

Actual behavior after `git mesh add <name> <path>` over the drifted
whole-file pin:

```
W CHANGED assets/hero.png#L0-L0
Pending mesh ops:
  ADD    assets/hero.png whole (…-uuid)  (drift: sidecar mismatch)
exit 1
```

Two issues:
1. The finding is **not** marked `(ack)` — exit code remains 1.
2. The pending op is spuriously flagged `(drift: sidecar mismatch)`
   even though `od -c` confirms the sidecar bytes are byte-identical to
   the current worktree and index content. The "mismatch" appears to
   compare sidecar against the anchor blob at the previous mesh commit
   — which is necessarily different for any re-anchor.

`git mesh commit <name>` **does** accept the re-anchor (exit 0), and
post-commit `stale` is clean. So the commit pipeline is correct; only
the pre-commit ack path is wrong. This would cause pre-commit hooks and
developer-facing `stale` runs to block intentional whole-file
re-anchors.

### Minor render drift

- Handbook example at `…handbook.md:623` shows human format with
  `(whole)` in place of the line range. Actual human output renders
  whole-file pins as `assets/hero.png#L0-L0`. Porcelain uses literal
  `0 0` in the line columns and `-` in the last column. Neither
  matches the handbook's `(whole)` token.

## Task 10 — Re-anchor semantics ⚠️ PARTIAL (2 bugs / doc contradictions)

- Overlapping-but-distinct ranges (`L1-L10` and `L5-L15`) both persist in
  the committed `ranges` tree. ✅
- Routine re-anchor without staging a new why inherits the previous
  commit message. ✅

### 🐛 BUG — duplicate staged `add` rejected instead of last-write-wins

Handbook `…handbook.md:290-294` and `…handbook.md:916-918` both promise
last-write-wins for duplicate `(path, extent)`:

> "The later op supersedes the earlier one (last-write-wins). No restore
> or rm is required."

Actual:

```
$ git mesh add m f.txt#L1-L10
$ git mesh add m f.txt#L1-L10
error: duplicate range location in mesh: f.txt:1-10
```

The second `add` is refused outright — the first staged op remains. If a
developer follows the handbook's advice to "re-stage the range to refresh
the sidecar" (Troubleshooting), they'll instead hit this error and the
sidecar will be frozen at the pre-drift bytes.

### 🐛 BUG — corrupted sidecar does not block commit or fail stale

Handbook §Exit code (`…handbook.md:491-492`) and §Atomicity
(`…handbook.md:1065`) say a sidecar/blob disagreement should fail
commit and be reflected in stale. Repro:

```
git mesh add m f.txt#L1-L10
echo tampered > .git/mesh/staging/m.1        # sidecar no longer matches the real slice
git mesh commit m                            # ✅ succeeds (should fail)
git mesh stale m --format=porcelain           # ✅ exit 0 (should surface)
```

Tamper detection is silently absent. A malicious (or buggy) editor of
the staging area can produce mesh commits whose sidecars lie about
what was anchored.

## Task 11 — Resolver config ✅ (copy-detection effect unproven)

- Default config: `copy-detection same-commit`, `ignore-whitespace false`. ✅
- `git mesh config <name> <key> <value>` stages; `git mesh commit`
  persists to the mesh's `config` tree entry. ✅
- Format tokens `%(config:copy-detection)`, `%(config:ignore-whitespace)`,
  `%(ranges:count)` all read back correctly.
- Unknown key rejected: `error: unknown config key 'bogus'`, exit 2.
- `--unset <key>` reverts to default.
- `ignore-whitespace true`: a whitespace-only edit (`5` → ` 5 `) within a
  meshed range no longer triggers drift (exit 0). ✅

**CONCERN — copy-detection modes appear to have no observable effect**:

A committed move of a distinctive 5-line block from `src.txt#L3-L7` to
`dst.txt#L2-L6` reports `CHANGED` on all three settings (`off`,
`same-commit`, `any-file-in-commit`). None surfaced MOVED or followed
the block. The block may be below git's default rename-detection
similarity threshold; I didn't exhaust the parameter space. This is
worth a deeper investigation against a richer fixture before trusting
the setting to change behavior in production.

## Task 12 — Why handling ✅

- First-commit-requires-why: `error: why required for first commit on mesh 'm'`, exit 2.
- `-m "text"` stages a why from argv.
- `-F path` reads from a file.
- `--edit` honors `GIT_EDITOR`; the edited buffer becomes the why.
- `git mesh why <name>` prints the current mesh's commit-message bytes.
- `git mesh why <name> --at <commit-ish>` walks mesh history; resolves
  `refs/meshes/v1/<name>~N` correctly.
- Re-anchor without a staged why inherits the prior why verbatim.

Matches handbook §Changing the relationship description and §Write a
useful why.

## Task 13 — Structural ops (mv / revert / delete) ✅

- `git mesh mv a b`: `refs/meshes/v1/a` deleted, `refs/meshes/v1/b`
  created with the same tip sha.
- `git mesh revert b <earlier-commit>`: creates a NEW mesh commit on top
  of current tip whose tree is byte-identical to the earlier commit's
  tree (same `config` and `ranges` blob OIDs). History preserved; three
  commits after revert.
- `git mesh delete b`: ref removed cleanly.
- `git mesh delete ghost`: `error: mesh not found: ghost`, exit 2.

Matches handbook §Renaming, deleting, and reverting.

## Task 14 — `git mesh ls` ✅

- Bare `git mesh` lists meshes with range counts and truncated why.
- `git mesh ls` enumerates every (path, mesh, range) triple.
- `git mesh ls <path>` filters by path.
- `git mesh ls <path>#L<s>-L<e>` uses inclusive overlap semantics:
  - Boundary `#L10-L10` and `#L20-L20` both match a `L10-L20` range.
  - Non-overlapping `#L25-L40` returns empty.
- Corrupting `.git/mesh/file-index` with garbage → `git mesh doctor`
  reports `FileIndexMissing: file index header missing or corrupt` and
  regenerates; subsequent `ls` works.

**DOC DRIFT — recommended hook body**: doctor still prints
`git mesh commit` for post-commit and `git mesh pre-commit-check` for
pre-commit. Consistent with the Task 1 finding; the handbook uses
`git mesh pre-commit` which is wrong.

## Task 15 — Format outputs ✅

All four formats produce usable output on a single-finding scenario
(worktree drift):

- **porcelain**: header `# porcelain v1`, then tab-separated rows.
- **json**: valid JSON, `schema_version: 1`, top-level `findings`,
  `pending`, `mesh`. Each finding has `range_id`, `anchored`, `current`,
  `source` ("WORKTREE"/"INDEX"/"HEAD"), `status.code`,
  `acknowledged_by` (null or UUID), `culprit`.
- **junit**: well-formed `<testsuite>` / `<testcase>` / `<failure/>`.
- **github-actions**: `::error file=<path>,line=<n>::CHANGED [<src>]`.

Matches handbook `…handbook.md:511-523`. `reason` under
`CONTENT_UNAVAILABLE` not exercised (blocked by Task 7 bug).

**Broken-pipe panic recurs** when piping `--format=junit` into
`xmllint` (or `head`). Consistent with the SIGPIPE issue noted in Task 3.
