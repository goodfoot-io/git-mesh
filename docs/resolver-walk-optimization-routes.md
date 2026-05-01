# Resolver walk optimization — Routes A, B, C

Date: 2026-04-30

## Constraints

- **No upstreaming to gix.** Every mechanism must work against the gix version pinned in this repo. In particular: `gix::diff::Options` exposes only `location` and `rewrites` — there is no pathspec or path filter on the tree-diff platform. Any path-restricted walk must be done outside gix.
- **Shelling out to `git` is acceptable.** It is already an established pattern in this crate: `mesh/path_index.rs:231`, `sync.rs:71`, `advice/workspace_tree.rs:87`. The first ranges-from-blob call site is `git diff-tree --raw -M`; this doc treats `git log --raw --name-only --follow` and friends as available primitives.
- **Greenfield.** No migrations, no fallbacks left behind. Replace cleanly.
- **Correctness first.** All three routes must produce byte-identical resolver outputs to the unoptimized walk, modulo new perf counters.

## Background

`git mesh stale` (workspace-wide) spends ~99% of its runtime building per-`anchor_sha` grouped walks. The dominant cost is `walker::name_status` — a rewrite-aware tree-diff per commit between `anchor_sha` and HEAD. Profiling on the local 4-mesh repo (commit `fb28151` baseline):

```
command.stale                 ≈ 3661 ms
resolver.resolve-anchors      ≈ 3618 ms (1990 + 1240 + 205 + 183)
session.ensure-calls          = 9
session.ensure-hits           = 5
session.walks-len             = 4   (75 + 76 + 8 + 126 commits = 285 unique)
```

Cross-mesh `ResolveSession` dedupe is already working (5/9 hit-rate). The remaining cost is the irreducible 285 unique commits × ~12 ms each.

A separate in-flight change implements **commit-skip**: `name_status` runs only on commits that touch a candidate path. Routes A, B, and C compose on top of commit-skip. They are evaluated for viability — whether the mechanism produces correct results under the constraints above. Effort is out of scope.

Prior art the routes are derived from:

- Elijah Newren's "irrelevant rename detection skipping" (git merge-ort, 2021).
- `gitdiffcore` and `remembering-renames` documentation in upstream git.
- `git log --follow` semantics for single-path rename trails.

## Route A — Two-pass walk to close the rename-trail hazard

### Issue

The in-flight commit-skip design seeds the candidate-path set with the mesh's anchor paths and widens it online by promoting `Renamed{from→to}.to` whenever an interesting commit is processed during a single forward pass.

The hazard, documented in upstream git's `remembering-renames`: if commit C₁ contains `foo→bar` (with `bar` not yet in the candidate set) and a later commit C₂ contains `bar→<anchored>`, the online widener picks up `bar` only at C₂. C₁ has already been classified non-interesting and skipped. The `foo→bar` rename is dropped from the trail; the anchor reports `Orphaned` instead of `Moved`.

### Plan

Replace the single online-widening pass with two passes per `(anchor_sha, copy_detection)`:

1. **Pass 1 — rename trail closure.** Build `closed_paths` and `interesting_commits` by iterating against git's pathspec-restricted rename-aware diff:

   1. **Seed.** Start with each anchor path P. P is the *anchor-time* name. `git log --follow` walks backward only, so seeding with anchor-time names alone misses the in-range `P → P'` rename when `P'` is the HEAD-time name.
   2. **Forward-resolve to HEAD-time name.** For each P, run

      ```
      git log --raw --name-only -M --no-color --format=%H \
          <anchor_sha>..HEAD -- <P>
      ```

      and chase every `Renamed{P → Q}` row forward, then `Q → R`, etc., until a name is reached that no later commit renames again. Add every name encountered (including P, intermediates, and HEAD-time name) to `closed_paths`.
   3. **Iterate to fixed point.** Re-run the same query with the pathspec replaced by the current `closed_paths`. Each pass may discover new renames whose `from` or `to` is now in the set; add them and repeat. Termination: `closed_paths` is monotone-increasing and bounded by `paths-touched-in-anchor..HEAD`, which is finite.
   4. **Copy-detection supplemental.** When the mesh's `copy_detection != Off`, run

      ```
      git log --raw -C --no-color --format=%H <anchor_sha>..HEAD
      ```

      (no pathspec — copies can come from anywhere). Add to `closed_paths` any `Copied{from, to}` whose `from ∈ closed_paths` *or* whose `to ∈ closed_paths`. Re-iterate (a copy may unlock further renames in the next iteration).

   Output:
   - `closed_paths: HashSet<String>` — every historical name any anchor path has had, including forward and backward across renames and copies.
   - `interesting_commits: HashSet<String>` — the commits whose `--raw` output touches any path in `closed_paths`.

   Pruning behavior to be aware of: with `--follow` or `-M`, git must run rename detection on full tree diffs *before* applying the pathspec filter. Subtree pruning only saves cost on commits that don't touch the spec's directories at all. The win is from skipping rewrite-aware diff in Pass 2 on the long tail of unrelated commits, not from Pass 1 being free.

2. **Pass 2 — full rewrite-aware diff on the closed set.** Run the existing rewrite-aware `name_status` (`walker.rs:270`) only on commits in `interesting_commits`. The candidate set never grows during Pass 2 — Pass 1 already discovered every name. Non-interesting commits get a `CommitDelta` with `entries: vec![]`; the consumer in `walker::advance_with_entries` already treats empty entries as `Change::Unchanged` (`walker.rs:79–138`). `compute_new_range` reads parent and commit blobs directly per call (`walker.rs:128`), so no cross-commit state accumulation makes the skip pattern unsafe.

### Where it touches

- `packages/git-mesh/src/resolver/session.rs` — `prepare_group` / `build_grouped_walk`. Add a `compute_rename_trail` helper that shells out to `git log`.
- `packages/git-mesh/src/git.rs` — add a `git_log_follow_paths` helper alongside the existing `Command::new("git")` call sites.
- The candidate-set online widener at `session.rs:262–274` (in-flight commit-skip code) is **deleted**. Pass 1 now provides the closed set up front; nothing widens during Pass 2.

### Constraints

- **Set agreement, not order agreement.** Pass 2 consumes `interesting_commits` as a `HashSet`. Pass 1 (`git log`) and Pass 2 (gix `rev_walk_excluding`) must produce the same *set* of commits in `anchor_sha..HEAD`. Both default to "all commits reachable from HEAD excluding `anchor_sha`'s ancestry"; set agreement holds. **Do not pin `--first-parent`.** It would skip side-branch commits today's resolver processes and is a regression.
- **Rename-budget detection.** Pass 1's `git log` honors git's `diff.renameLimit` (configure to match `GIT_MESH_RENAME_BUDGET` for the subprocess invocation). On overflow, git emits to stderr the literal string `warning: exhaustive rename detection was skipped due to too many files.` (verified against git 2.39.5). On detection of that warning, fall back to `interesting_commits = all commits in anchor_sha..HEAD` for that group — do not silently shrink the trail. Pure-OID renames (R100) succeed silently under any limit, which is fine — no information lost.
- **Filter rename rows by current `closed_paths`, not the seed.** Each fixed-point iteration reads the previous iteration's `closed_paths`. New renames whose `from` or `to` enters the set during iteration N must be picked up in iteration N+1. Spell this out in the implementation: the iteration variable is `closed_paths`, not the original seed.
- Cache the Pass 1 output inside `GroupedWalk` so it can feed Route C.

### Tests

- Fixture exercising the exact hazard: anchor at HEAD~50, a `foo→bar` rename in HEAD~30 with `bar` not yet anchored, a `bar→<anchored>` rename in HEAD~10. Assert the anchor resolves as `Moved` (not `Orphaned`) and the trail captures both renames.
- Copy-detection variant: copy from anchored path at HEAD~20 with `copy_detection = AnyFileInRepo`. Assert the destination is added to the candidate set.
- `tests/cli_stale_*.rs`, `tests/commit_mesh_integration.rs`, `tests/slice_3_render.rs` continue to pass with byte-identical output.
- New unit test for `compute_rename_trail` covering: single anchor path, multiple anchor paths, anchor path not present at HEAD, rename-budget exhaustion → fall back to all commits.

### Viability

- **Correctness:** the two-pass design closes the documented hazard if and only if the iteration is seeded with both anchor-time and HEAD-time names and re-runs against `closed_paths` until fixed point. The naive "single `--follow` per anchor path" approach (the original draft of this plan) produces a *worse* trail than today's online widener, because `--follow` walks backward only and misses in-range forward renames.
- **Mechanism:** `git log --raw -M` and `git log --raw -C` are stable, in-tree primitives. No gix changes required.
- **Subtree pruning is weaker than naively assumed.** With `--follow` or `-M`, git runs rename detection on full tree diffs *before* applying the pathspec filter. Subtree pruning only saves cost on commits that don't touch the spec's directories. The Pass 2 skip is the dominant win, not Pass 1 cost reduction.
- **Failure modes:** rename-budget exhaustion produces a stable stderr warning we detect; on detection, fall back to "all commits interesting" for that group. Fixed-point iteration terminates because `closed_paths ⊆ paths-touched-in-range`, which is finite.
- **Pass-2 baseline correctness:** verified — `walker::advance_with_entries` (`walker.rs:79–138`) returns `Change::Unchanged` for empty `entries` without calling `compute_new_range`; `compute_new_range` reads parent and commit blobs directly, no cross-commit state.

**Verdict: viable, given the seeded-and-iterated trail closure described above.** The original "one `--follow` per anchor path, union the results" formulation is not viable.

## Route B — Local two-stage pipeline for in-commit rewrite filtering

### Issue

After Route A lands, `name_status` cost lives inside Pass 2 — full rewrite-aware diff on the small interesting set. On commits with large changesets (mass renames, formatter sweeps, generated-code regenerations), the O(n²) similarity matrix inside gix's `Rewrites` tracker still dominates *for that single commit*. Upstream git's "irrelevant renames skipping" (Newren, 2021) drops paths from the matrix when their rename status cannot influence the answer the caller cares about. We can't add this filter to gix, but we can implement an equivalent two-stage pipeline locally.

### Plan

Replace the single `collect_changes(..., copy_detection, true)` call inside `name_status` with a two-stage local pipeline, applied per commit on the Pass 2 hot path:

1. **Stage 1 — cheap diff (no rewrites).** Call `collect_changes(..., copy_detection, false)` on the (parent, commit) tree pair. This walks both trees but skips the rewrite tracker entirely. Output: raw `Added{path}`, `Deleted{path}`, `Modified{path}` rows. Cost dominated by tree walk, not similarity scoring.
2. **Stage 2 — neighborhood filter.** Compute the *candidate-set neighborhood* for this commit:
   - `keep_deletes = { d.path | d.path ∈ closed_paths }`
   - `keep_adds = { a.path | a.path ∈ closed_paths } ∪ ({ all adds } if copy_detection != Off else ∅)`

   If `keep_deletes` and `keep_adds` are both empty, return Stage 1's output projected through `project_to_no_rewrites` (no rename rows possible). Skip Stage 3.
3. **Stage 3 — targeted similarity matrix matching gix's greedy bucketed algorithm.** Build a similarity matrix only over `keep_deletes × keep_adds` (plus, for `copy_detection != Off`, the source set defined below). Stage 3 must replicate gix's `Rewrites { copies, percentage: 0.5, limit: 1000, track_empty: false }` algorithm exactly — it is *not* a maximum-weight bipartite match. Specifically:

   - **Greedy first-match-above-threshold.** For each unmatched destination iterated in tree-diff emission order, score against candidate sources iterated in tree-diff emission order; the *highest-scoring* match `>= 50%` wins, with ties broken by source emission order. Reading `gix-diff/src/rewrites/tracker.rs` against the implementation before writing the differential test is required.
   - **Size bucketing.** gix buckets blobs by size and only scores cross-bucket pairs above a size-ratio threshold. Replicate the same bucketing.
   - **`track_empty: false`.** Zero-byte blobs are excluded from being either source or destination of a rewrite. Filter them out of both `keep_deletes` and `keep_adds` before scoring.
   - **Raw bytes, no normalization.** Similarity scoring uses raw blob bytes; do not apply `core.autocrlf` or attribute-driven normalization. Use `gix::diff::blob` on the raw OIDs.
   - **`Vec<NS>` ordering matches gix's emission order.** Consumer `walker::advance_with_entries` (`walker.rs:89`) iterates entries linearly and lets later rows overwrite earlier decisions for the same path. Stage 3 must emit `Renamed`/`Copied` rows in the same position gix would emit them.

   Validate every requirement above by running both implementations on a corpus and asserting equal `Vec<NS>` outputs byte-for-byte.

### Where it touches

- `packages/git-mesh/src/resolver/walker.rs` — replace the body of `name_status` (`walker.rs:270`). The current `collect_changes(..., true)` + `widen_copies_*` machinery is replaced by Stage 1 + Stage 2 + Stage 3.
- New helper module `packages/git-mesh/src/resolver/similarity.rs` — wraps `gix::diff::blob` for pairwise similarity scoring. Encapsulates the matrix construction so Stage 3 stays small.
- `widen_copies_in_commit` / `widen_copies_any_ref` (`walker.rs:319, 323`) — fold their logic into Stage 3, which already enumerates copy candidates.

### Constraints

- **Equivalence to current `name_status`.** Stage 3's similarity scoring must match `gix::diff::Rewrites { percentage: 0.5, .. }`'s pairing decisions. Any divergence changes user-visible `Renamed`/`Copied` classifications. Validate with a differential test: a corpus of (parent, commit) tree pairs run through both implementations; assert identical `Vec<NS>` outputs.
- **Copy detection levels.** `CopyDetection::SameCommit`, `AnyFileInCommit`, and `AnyFileInRepo` change which blobs are eligible as copy sources. Stage 3 must enumerate the same source set per level: respectively, parent-tree blobs that are added at the same commit (pruned by construction), parent-tree blobs at this commit, or any blob anywhere in any ref. For `AnyFileInRepo`, replicate `widen_copies_any_ref` (`walker.rs:323`) and its helper `all_ref_blob_paths` (`walker.rs:466`): iterate refs in `repo.references().all()` order, dedupe by OID with the *first-encountered* path winning per OID. Determinism depends on this exact iteration.
- **Rename budget.** Keep today's gate semantics: trigger fallback when `derived_no_rewrites_count(&entries) > GIT_MESH_RENAME_BUDGET` (`walker.rs:301, 432`), measured against the *full* projected changeset, not the matrix size `|keep_deletes| × |keep_adds|`. This preserves the existing user-visible warning trigger so users don't see warnings appear or disappear because of Route B. When the gate passes, Stage 3 runs a smaller matrix (the win); when it fails, fall back to projecting Stage 1 through `project_to_no_rewrites` and emit the same warning the current path emits (`walker.rs:303`).
- **No similarity work for non-interesting commits.** Stage 1's neighborhood check returns early when neither side intersects the candidate set. This is the win — most Pass 2 commits with large changesets but only a few candidate paths now skip the matrix entirely.

### Tests

- Differential test as described above: gix `Rewrites` tracker output vs. local two-stage pipeline output, byte-identical on a corpus drawn from this repo's history (the 285 commits in the existing walks plus a synthetic large-changeset fixture).
- Mass-rename fixture: 200-file rename in one commit with only one file in `closed_paths`. Assert Stage 3's matrix is 1×1, not 200×200, and the result still pairs the relevant rename correctly.
- Copy-detection coverage: each of `Off`, `SameCommit`, `AnyFileInCommit`, `AnyFileInRepo` exercised against fixtures with copies of anchored paths. Assert classifications match the gix-only baseline.
- `tests/cli_stale_*.rs`, `tests/commit_mesh_integration.rs`, `tests/slice_3_render.rs` continue to pass with byte-identical output.

### Viability

- **Correctness:** the optimization is a strict subset of what upstream git already ships in its merge-ort engine; the math is well-understood.
- **Mechanism:** gix exposes blob-level similarity via `gix::diff::blob` (used elsewhere in this crate for hunk math). We can build the matrix ourselves.
- **Equivalence to gix `Rewrites`:** the only viability risk. If gix's tracker does anything beyond pairwise similarity scoring at 50% threshold (e.g. a different greedy algorithm, a different normalization, a different copy-source enumeration order), our reimplementation will diverge. The differential test is the authoritative check.
- **Composability with Routes A and C:** Route A populates `closed_paths` and `interesting_commits` before Pass 2 runs; Route B operates inside Pass 2 on a per-commit basis. They don't interact. Route C caches Pass 1 output; Route B doesn't touch the cache.
- **Open question:** does gix's `Rewrites { copies: Some(Copies::default()), percentage: 0.5 }` produce stable, deterministic pairings on ambiguous matrices (multiple deletes that all match one add equally)? If yes, our reimplementation must match the same tie-breaking. If no, the differential test needs a tolerance — but `Vec<NS>` ordering is consumer-visible (`walker::advance_with_entries` uses first-match), so we'd have a real divergence to manage.

**Verdict: viable, contingent on the differential test passing.** If gix's `Rewrites` pairing diverges from a straightforward 50%-similarity bipartite match in any way that affects classification, Route B requires either matching the divergence exactly or accepting a behavior change.

## Route C — Cross-invocation rename-trail cache

### Issue

`ResolveSession` is per-invocation. Every `git mesh stale` (and every advice flush that triggers a resolver run) rebuilds the rename trail and the per-commit deltas from scratch, even when neither HEAD nor the anchor SHAs have moved since the last invocation. Hooks invoke `git mesh stale` and `git mesh advice` frequently; the trail is the same each time.

`compact` advances `anchor_sha` to HEAD when an anchor is clean, which already reduces walk depth — but between compacts, repeated invocations pay the full Pass 1 + Pass 2 cost on every call.

### Plan

Persist Pass 1 output (the rename trail and the interesting-commit set) to `.git/mesh/cache/rename-trail/v1/<anchor_sha>.json`. The directory is versioned (`v1/`) so an older client and a newer client with a different schema cannot collide on the same key — each writes into its own version directory.

**Cache key** (every component must match for a hit):
- `anchor_sha`
- `head_sha`
- `copy_detection`
- `rename_budget` (read from `GIT_MESH_RENAME_BUDGET` at run time)
- `candidate_seed_hash` — SHA-256 of the sorted, newline-joined seed paths
- `replace_refs_hash` — SHA-256 of `git for-each-ref refs/replace/`'s output, or empty if no replace refs
- `git_config_hash` — SHA-256 of `git config --get-all` output (concatenated, sorted) for the keys that affect Pass 1's rename detection: `diff.renames`, `diff.algorithm`, `diff.renameLimit`, `core.ignoreCase`, `core.precomposeUnicode`. These influence the diff git's pathspec engine produces and therefore which commits land in `interesting_commits`.

If any component differs, miss and recompute.

**Schema** (line-oriented; written under `.git/mesh/cache/rename-trail/v1/`):

```
anchor_sha <hex>
head_sha <hex>
copy_detection <off|same-commit|any-file-in-commit|any-file-in-repo>
rename_budget <integer>
candidate_seed_hash <hex>
replace_refs_hash <hex>
git_config_hash <hex>
seed <path>
seed <path>
...
closed <path>
closed <path>
...
interesting <commit_sha>
interesting <commit_sha>
...
```

The schema version is encoded in the directory path, not in the file body. A future schema change writes to `v2/`; older clients reading from `v1/` and newer clients reading from `v2/` coexist without thrashing each other.

**On `prepare_group`:**

1. Compute current cache key.
2. If `.git/mesh/cache/rename-trail/<anchor_sha>.json` exists and parses cleanly:
   - If every key field matches, load `closed_paths` + `interesting_commits` and skip Pass 1.
   - Else, miss.
3. On miss, run Pass 1 and write the result. Write goes through `<file>.tmp.<pid>` → `fsync` → `rename`. POSIX rename is atomic on the same filesystem; concurrent writers produce one winner with no torn reads. No flock — the cache content is deterministic given the key, so a "lost" write is acceptable (next miss recomputes).
4. On parse failure or schema mismatch, treat as miss and overwrite.

**Lifecycle:**

- `compact` clears `.git/mesh/cache/rename-trail/v1/<old_anchor_sha>.json` after a successful advance. Old `anchor_sha` is no longer referenced by any mesh; cache file is dead weight.
- New `git mesh doctor --gc-trail-cache` walks every versioned subdirectory under `.git/mesh/cache/rename-trail/`, lists every `<sha>.json`, and deletes any whose `<sha>` is not currently an `anchor_sha` of any live mesh ref. The same sweep also reaps stale tempfiles (`*.tmp.*`) older than 1 hour — crash mid-write leaves them lingering, and they otherwise have no other cleanup trigger. Run on user demand; not gated on every invocation.
- Cache failure (read error, parse error, write error) is logged at perf level and treated as a miss. Never fatal.
- GC'd `anchor_sha`: cache file content does not dereference the SHA — it only keys on it. So a cache hit for a GC'd anchor returns correct data, but no caller will ever query that key (the mesh ref pointing at it is gone or has moved). Detection: `git cat-file -e <sha>^{commit}`. The doctor sweep deletes such files on its own pass; no special-case handling needed during reads.

### Where it touches

- New module `packages/git-mesh/src/resolver/trail_cache.rs` — read/write/serialize.
- `packages/git-mesh/src/resolver/session.rs` — `prepare_group` calls `trail_cache::load_or_compute`.
- `packages/git-mesh/src/mesh/compact.rs` — call `trail_cache::clear(old_anchor_sha)` after successful advance.
- `packages/git-mesh/src/cli/doctor.rs` (or wherever doctor lives) — add `--gc-trail-cache` subcommand.

### Constraints

- **Concurrency.** Tempfile-then-rename pattern as above. Multiple concurrent invocations may each compute Pass 1; one winner replaces the file atomically. Acceptable.
- **Replace refs.** `repo.find_commit` honors `refs/replace/*`. Adding/removing a replace ref between runs changes what `name_status` sees on cached commits. The `replace_refs_hash` key field invalidates on change.
- **Rename budget.** Captured in cache key; budget changes invalidate.
- **Candidate seed drift.** Two meshes can share an `anchor_sha` but have different anchored paths. If the cache was written by mesh A's invocation and mesh B's invocation reads it, the cached `closed_paths` may miss paths B cares about. The `candidate_seed_hash` key field invalidates on this. (Alternative: store the seed and check that the current invocation's seed is a subset; only useful if cache files for the same `anchor_sha` are common across meshes. Keep it simple: hash invalidation.)
- **Garbage-collected `anchor_sha`.** On read, if `repo.find_commit(anchor_sha).is_err()`, treat as miss and delete the cache file.

### Tests

- Unit test `trail_cache::roundtrip` — write then read returns the same data.
- Unit test for each cache-key field: change `head_sha`, `rename_budget`, `candidate_seed_hash`, `replace_refs_hash`, `copy_detection`; assert miss in each case.
- Integration test: run `git mesh stale` twice, assert the second run hits the cache (`session.trail-cache-hits = walks-len`).
- Integration test: invalidation — run, advance HEAD with a commit, run again, assert the cache misses for affected anchor SHAs.
- Integration test: `compact` clears the obsolete trail cache for the old `anchor_sha`.
- Integration test: `git mesh doctor --gc-trail-cache` removes orphaned files but preserves live ones.

### Viability

- **Correctness:** the cache key covers every input that affects Pass 1's output. No reachable-state mutation produces a stale-but-keyed hit.
- **Mechanism:** `.git/mesh/cache/` is per-clone, gitignored, never pushed. Tempfile-rename for concurrency. Standard pattern.
- **Composability:** Route C caches Pass 1 output. Routes A and B run downstream of Pass 1; the cache is invisible to them. The cache schema includes everything Pass 2 needs to reconstruct.
- **`replace_refs_hash` is worth keeping.** Cheap to compute (`git for-each-ref refs/replace/`); silent staleness from a forgotten `git replace` would cost hours to diagnose. Submodule pointer changes are not a key input — `walker.rs` does not recurse into submodules.

**Verdict: viable.**

## Sequencing

1. **Background agent's commit-skip** (in flight). Foundation.
2. **Route A.** Closes the rename-trail correctness hazard. Must ship before any production reliance on commit-skip.
3. **Route C.** Independent perf win on top of Route A.
4. **Route B.** Composes orthogonally; ship when Pass 2 cost on real workloads warrants it. Differential-test gate must pass first.

## Validation plan

For each route under `packages/git-mesh`:

```
cd packages/git-mesh
yarn lint
yarn typecheck
yarn test
cd ../..
yarn validate
```

Then re-profile:

```
GIT_MESH_PERF=1 git mesh stale 2>&1 | grep "git-mesh perf:" | sort -k4 -rn
```

Report `command.stale`, `resolver.resolve-anchors` per-mesh totals, and the new `session.*` counters in the PR description. Counter naming: kebab-case throughout (`session.skipped-commits`, `session.interesting-commits`, `session.trail-cache-hits`, `session.trail-cache-misses`, `session.pass1-ms`, `session.pass2-ms`).
