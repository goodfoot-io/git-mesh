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

## Task 16 — Exit-code contract ✅

All eight scenarios match handbook §Exit code (`…handbook.md:485-500`):

| Scenario | Expected | Actual |
|---|---|---|
| FRESH | 0 | ✅ 0 |
| Worktree drift | 1 | ✅ 1 |
| Worktree drift + `--no-exit-code` | 0 | ✅ 0 |
| Index drift | 1 | ✅ 1 |
| Index drift + staged re-anchor (ack) | 0 | ✅ 0 |
| HEAD drift (HEAD-only mode) | 1 | ✅ 1 |
| HEAD drift + `--no-exit-code` | 0 | ✅ 0 |
| PendingFinding::Why only | 0 | ✅ 0 |
| PendingFinding::ConfigChange only | 0 | ✅ 0 |

Terminal statuses (ORPHANED, MERGE_CONFLICT) confirmed exit 1 in Task 7.
CONTENT_UNAVAILABLE / `--ignore-unavailable` cross-product blocked by
Task 7 LFS bug. Sidecar-mismatch exit-code behavior cannot be verified
due to the Task 10 bug (tamper detection is absent).

## Task 17 — Sync push/fetch ✅ (minor: duplicate refspecs)

- `git mesh push` on first use populates `remote.origin.fetch` with
  `+refs/ranges/*:refs/ranges/*` and `+refs/meshes/*:refs/meshes/*`.
- Remote `refs/meshes/v1/*` and `refs/ranges/v1/*` are pushed; a fresh
  clone's `git mesh fetch` pulls them and renders correctly.
- `git config mesh.defaultRemote <alt>` + `git mesh push` targets the
  alternate remote.

**MINOR BUG — duplicate refspec entries**: after multiple `git mesh push`
calls the config grows:

```
+refs/heads/*:refs/remotes/origin/*
+refs/ranges/*:refs/ranges/*
+refs/meshes/*:refs/meshes/*
+refs/ranges/*:refs/ranges/*      # duplicate
+refs/meshes/*:refs/meshes/*      # duplicate
```

Not functionally harmful (git tolerates dups) but clutters the config
and `doctor` output. Lazy-config should be idempotent.

## Task 18 — `--since` branch scope 🐛 BUG

`git mesh stale --since <commit-ish>` documents (`--help` and handbook
§CI patterns) "Only ranges anchored at or after this commit". In
practice it has **no observable effect**:

Setup: `on-main` anchored at `main@{seed}`, `on-feat` anchored one
commit later on `feat`. Commit further drift so both meshes are
CHANGED vs HEAD.

| `--since` arg | Expected findings | Actual |
|---|---|---|
| (absent) | on-main, on-feat | ✅ both |
| `merge-base feat main` | on-feat only | ❌ both shown |
| `HEAD` | none | ❌ both shown |
| first seed commit | on-feat only (on-main anchored at the seed itself; semantics "at-or-after" is ambiguous) | ❌ both shown |

Without `--since` working, the handbook's PR-gate recipe
(`…handbook.md:525-530`, `…handbook.md:821-829`) cannot scope CI runs to
"ranges anchored on the current branch" and will fail on historical
drift outside the PR's scope.

## Task 19 — Concurrency guard ✅

Running `git mesh stale` while a background loop hammered `git add`:

```
warning: index changed during stale; consider re-running
mesh m
0 stale of 1 ranges:
stale-exit=0
```

- Stderr warning emitted (`index changed during stale; consider
  re-running`) matching handbook §Resolver line 1058.
- Exit code unaffected (0 here because the range happened to stay
  fresh). Handbook's "exit code unaffected" claim verified.

## Task 20 — Atomicity / CAS ⚠️ PARTIAL

- Sequential commits produce a linear chain on `refs/meshes/v1/m` with
  each new commit parented at the prior tip. ✅
- True CAS retry under a race is hard to exercise from the CLI because
  `.git/mesh/staging/` is shared in-process state: two parallel
  `git mesh commit` calls don't race on the ref update — the first
  consumes all staged ops and the second reports `error: nothing
  staged`. A real CAS test would need independently-computed trees
  pushed at the same tip (e.g., concurrent `git push` of mesh refs) or
  an internal test-only hook.
- Custom refs under `refs/meshes/*` do not get reflog entries by default
  (git's `core.logAllRefUpdates = true` only logs refs under
  heads/remotes/notes). Handbook §Atomicity mentions reflog safety;
  teams who want mesh reflogs must explicitly
  `git config core.logAllRefUpdates always`.

## Task 21 — Doctor recovery ✅ (sidecar blind-spot)

| Probe | Expected | Actual |
|---|---|---|
| Missing `file-index` | regenerate | ✅ `FileIndexMissing: … regenerating automatically` (Tasks 1, 14) |
| Missing hooks | report | ✅ `MissingPreCommitHook`, `MissingPostCommitHook` |
| Dangling `refs/ranges/v1/<id>` | report | ✅ `DanglingRangeRef: range … not referenced by any mesh` |
| Missing remote refspec | report | ✅ `RefspecMissing: remote 'origin' has no mesh refspec` |
| Corrupted sidecar bytes | report | ❌ **silent** (consistent with Task 10 — no sidecar integrity check anywhere in the pipeline) |

Doctor is otherwise reliable. The sidecar blind-spot is the same
vulnerability flagged in Task 10.

## Task 22 — Reserved names ✅

All 16 names listed at `…handbook.md:1129-1131` are refused with
`error: reserved mesh name: <name>`:

```
add, rm, commit, why, restore, revert, delete, mv, stale, fetch,
push, doctor, log, config, ls, help
```

## Task 23 — `--at` anchoring ✅ (with arg-order quirk)

- `git mesh add <name> --at <commit-ish> <range>` anchors the stored
  `anchor` field in the range blob to the specified commit (confirmed
  by `git cat-file blob refs/ranges/v1/<uuid>`).
- Post-commit hook flow: without `--at`, the range resolves at mesh-commit
  time to the current HEAD. After an intervening source `git commit`,
  the post-commit hook fires and the stored anchor equals the new HEAD.
  ✅

**MINOR BUG — `--at` only works before positional ranges**

```
git mesh add old f.txt#L1-L1 --at HEAD~2
  → error: path not in tree: f.txt at HEAD~2

git mesh add old --at HEAD~2 f.txt#L1-L1
  → works
```

clap accepts both orderings syntactically, but the handler only honors
`--at` when it appears before ranges. Every handbook example happens to
put `--at` first, which hides the bug. Either reject the misordered
invocation with a clearer error or make the parser order-independent.

## Task 24 — Log traversal cleanliness ⚠️ PARTIAL

- `git log --all --oneline` includes mesh commits. ✅
- `git log --branches --remotes --tags --oneline` excludes them — this
  is the right recipe for "normal" history. ✅
- `log.excludeDecoration refs/meshes/*` suppresses decoration badges on
  mesh commits but does not remove them from the log. ✅

### 🐛 DOC BUG — `--exclude=refs/meshes/*` does nothing

Handbook `…handbook.md:965-966`:

```bash
git log --all --exclude=refs/meshes/*
```

Repro: a repo with one mesh commit. `git log --all --oneline` shows
three commits (two source + one mesh). Adding `--exclude=refs/meshes/*`
to the same command prints the **same three commits**. `--exclude`
applies to subsequent `--all`/`--branches` expansion only with specific
syntax and globbing rules; this invocation doesn't actually exclude.

The working recipes are:
- `git log --branches --remotes --tags` (scoped traversal), or
- `git for-each-ref --format='%(refname)' refs/heads refs/remotes refs/tags | xargs git log` (explicit).

### Minor: range-ref log behavior

Handbook claim "Range refs point at blobs, so log traversal does not walk
them as commit history" (`…handbook.md:969`). True in effect:
`git log refs/ranges/v1/<uuid>` produces **no output and exits 0**
(not an error as the sentence might imply). Consistent with git's
behavior on commitless refs.

---

## Summary

### Critical bugs (fix before relying on the handbook)

1. **Task 7/8**: `git mesh commit` rejects any staged **LFS line-range**
   (`error: invalid range: start=N end=M`). Validation reads the LFS
   pointer blob instead of filtered content. Breaks §Pinning LFS-managed
   files and the LFS worked example end-to-end.
2. **Task 9**: Whole-file staged re-anchor is not marked `(ack)` and
   exits 1 with spurious `(drift: sidecar mismatch)`, even though the
   sidecar matches current bytes. Blocks the re-anchor ack flow for all
   whole-file pins (images, symlinks, gitlinks).
3. **Task 10**: Duplicate `(path, extent)` staged `add` is rejected with
   `error: duplicate range location in mesh` instead of last-write-wins.
   Contradicts handbook re-anchor guidance in multiple places.
4. **Task 10**: Corrupted staging sidecar passes `git mesh commit` and
   `git mesh stale` silently. No integrity check. Doctor is equally
   blind (Task 21).
5. **Task 18**: `--since <commit-ish>` has **no effect**. Breaks the
   documented PR-gate CI recipe.

### Doc bugs (update the handbook)

- **Task 1/14**: Hook-installation snippet recommends `git mesh
  pre-commit`; the correct subcommand is `git mesh pre-commit-check`.
- **Task 5**: §Understanding stale output claims a drifting range
  surfaces "once per drifting layer." Actual resolver emits one finding
  at the shallowest drifting layer.
- **Task 6**: porcelain v1 output drops the `src` column when all
  non-HEAD layers are disabled. Document or normalize.
- **Task 9**: whole-file render uses `#L0-L0`, not the documented
  `(whole)` token.
- **Task 24**: `git log --all --exclude=refs/meshes/*` doesn't exclude
  anything. Recommend `--branches --remotes --tags` instead.
- **Task 11** (minor): §Changing resolver settings implies
  copy-detection modes change behavior, but a small fixture couldn't
  demonstrate any difference.
- **Task 1** (minor): staging layout description omits the `.meta`
  sidecars.

### Minor / quality issues

- **Task 3, 15**: CLI panics on SIGPIPE (`failed printing to stdout:
  Broken pipe`) when output is piped into `head`, `less`, or similar.
  Install a SIGPIPE handler / `ignore_result` on stdout writes.
- **Task 4**: binary detection is `.gitattributes`-only, not
  content-based. Tighten or document.
- **Task 17**: `git mesh push` duplicates refspec entries on repeat
  calls. Lazy config should be idempotent.
- **Task 20**: custom refs don't get reflog entries unless
  `core.logAllRefUpdates=always`. Worth mentioning in §Atomicity.
- **Task 23**: `--at` flag only works before positional ranges.

### What worked well

- Core mesh lifecycle: `add → why → commit → stale → mv/revert/delete`.
- Layered stale resolver with correct `src` attribution.
- Status transitions: FRESH, MOVED, CHANGED, ORPHANED, MERGE_CONFLICT.
- Format outputs: human, porcelain, json (schema_version=1), junit,
  github-actions.
- Exit-code contract for the tested matrix.
- Config persistence and format-token readback.
- Sync over a bare remote, including lazy refspec bootstrap and
  `mesh.defaultRemote`.
- Pre-commit hook discrimination between index-layer and worktree-layer
  drift.
- Concurrency stderr warning on index race.
- Doctor coverage for missing hooks, dangling ranges, missing file-index,
  missing refspec.
- `ls` overlap semantics and reserved-name enforcement.

### Deferred

- CONTENT_UNAVAILABLE reasons (`PromisorMissing`, `SparseExcluded`,
  `FilterFailed`, `LfsNotInstalled`) — require specialized fixtures and
  blocked partly by Task 7 bug.
- SUBMODULE terminal status — cannot be constructed because the CLI
  correctly refuses new line-range pins inside submodules; would need a
  legacy/imported range.
- True CAS-retry atomicity — shared staging directory prevents
  constructing a race from the CLI.
