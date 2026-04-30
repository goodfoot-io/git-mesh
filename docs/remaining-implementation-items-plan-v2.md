# Remaining Implementation Items Plan — v2

Date: 2026-04-30

## Scope

The eight items from the prior plan (advice flush/read candidate scoping, candidate resolver API, compact staging predicate, shared resolver state, already-at-HEAD fast path, no-op path-index writes, dequadratic compact) are all implemented in `main` (commits `06d54fc` and `30226b5`); tests for each are present and passing.

This plan captures gaps and optimizations surfaced during that review plus during the related ignored-test cleanup. Each item lists the smallest change that resolves it.

## Summary

1. **Dead code in resolver and advice/candidates.** `resolver::all_meshes`, the six `Detector`-trait wrappers in `advice/candidates.rs`, and the per-tool detector functions are no longer called from any production code path. Either delete them or wire them back into the new flush/read flow with documented behavior.
2. **Quadratic dedupe in `candidate_mesh_names_for_paths`.** The helper uses `Vec::contains` per insert; switch to insertion-ordered `HashSet` for path-heavy flushes.
3. **CAS-conflict fallback throws away shared `EngineState`.** When `compact_meshes_batch` hits a CAS conflict on a single mesh, the retry path constructs a fresh `EngineState` from scratch instead of cloning the HEAD-blob cache from the batch state.
4. **`run_advice_flush` recomputes candidates per touch.** The per-touch loop emits suggestions through `meshes`, but the candidate set is already deduped above; the inner overlap check is O(meshes × touches) and could be O(touches × candidate_meshes) with a small index.
5. **Stale doc references.** `docs/remaining-implementation-items-plan.md` describes future work for items now complete; carry it forward into a "completed" rollup or delete in favor of v2.

## Item 1: Remove or revive dead code in resolver/engine and advice/candidates

### Current Issue

After the path-index candidate workflow landed, two surfaces became unreachable from production code:

1. `resolver::engine::all_meshes` (`packages/git-mesh/src/resolver/engine/mod.rs:380`). Its only previous callers were `run_advice_flush` and `run_advice_read`; both now go through `resolve_named_meshes`. No source or test invokes `all_meshes` today.
2. The six `Detector`-trait wrappers in `packages/git-mesh/src/advice/candidates.rs:849-934` (`PartnerDriftDetector`, `ReadIntersectsMeshDetector`, `StagingCrossCutDetector`, `DeltaIntersectsMeshDetector`, `RangeShrinkDetector`, `RenameConsequenceDetector`). The only `Detector::detect` caller is `SuggestDetector::detect` which immediately `bail!`s. The wrappers and the underlying `detect_*` functions are reachable only from their own unit tests.
3. The `detect_*` debug tags (`detect_partner_drift`, `detect_delta_intersects_mesh`, etc.) inside `crate::advice_debug!` calls in `candidates.rs` therefore never fire in production runs. Only the `suggester-*` tags emitted by `advice/suggest/mod.rs` and `advice/suggest/emit.rs` are observable when `GIT_MESH_ADVICE_DEBUG=1`.

### Recommendation

Pick one of the two endpoints and commit to it:

- **Delete path.** Remove `all_meshes`, the six `Detector` wrappers, and the `detect_*` functions if the resolver-based flush/read pipeline is the only intended advice surface. Keep `SuggestDetector` and the suggester pipeline; they are exercised by `flush` for Added paths and by `git mesh advice <sid> suggest`.
- **Revive path.** Wire the six detectors into `run_advice_flush` so per-tool partner drift and staging-cross-cut output reaches the agent inline (matching the original card-22 acceptance signals more closely). This is more work and likely overlaps the resolver-based flow; only do it if a missing signal is observed in real sessions.

Default recommendation: delete. The new resolver-based flow reproduces the user-visible signal (mesh partner advice on edits and reads) and the corpus-wide suggester covers cross-session NewMesh suggestions. Keeping unreachable detector code burns review attention with no payoff.

### Implementation Notes

- After deletion, drop `Detector` trait re-exports in `advice/mod.rs` and the now-unused `pub use` lines.
- Inline `detect_range_shrink` (returns empty Vec) is easy collateral; remove with the others.
- If the delete path is taken, also drop `GIT_MESH_ADVICE_DEBUG` integration coverage of `detect_*` tags. The debug.rs unit tests for `format_line`/`escape_value` already cover the format itself.

### Tests

- `cargo test -p git-mesh` after deletion. The unit tests under `mod tests` in `candidates.rs` need to be removed alongside the functions they exercise. The cross-suite suggester tests under `tests/advice_suggest_*.rs` are unaffected.
- Run `yarn validate` to confirm the workspace is still green.

## Item 2: Replace `Vec::contains`-based dedupe in `candidate_mesh_names_for_paths`

### Current Issue

`candidate_mesh_names_for_paths` in `packages/git-mesh/src/cli/advice/mod.rs:109-124` does:

```rust
for (path, range) in paths {
    let names = matching_mesh_names(...).unwrap_or_default();
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
}
```

For a flush that touches many paths cross-attached to many meshes (path × mesh), this is O(n²) on the candidate count. In practice it is small, but the upstream goal of v1 Item 1 was to keep advice cost path-bounded.

### Recommendation

Use an insertion-ordered set:

```rust
use std::collections::HashSet;
let mut seen: HashSet<String> = HashSet::new();
let mut out: Vec<String> = Vec::new();
for (path, range) in paths {
    for name in matching_mesh_names(repo, path, range).unwrap_or_default() {
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
}
```

This preserves deterministic order and drops dedupe to amortized O(1) per insert.

### Tests

The existing `tests/advice_path_index_candidates.rs` covers correctness. Add a focused test only if instrumentation flags a regression in candidate ordering.

## Item 3: Carry shared `EngineState` into single-mesh CAS retry

### Current Issue

`compact_meshes_batch` (`packages/git-mesh/src/mesh/compact.rs:530-589`) hands a shared `EngineStateHandle` to each per-mesh attempt. On CAS conflict it falls back to `compact_mesh_with_retry` with `fresh_tip`; that helper calls `resolve_mesh_at(repo, name, options, &current_tip)` which builds a brand-new `EngineState` per attempt. The HEAD blob cache and any anchor walk caches accumulated by the batch state are discarded for the conflicted mesh.

### Recommendation

Extend `compact_mesh_with_retry` to accept an optional `&mut EngineStateHandle` borrowed from the caller. The single-mesh entrypoint (`compact_mesh`) keeps the current behavior of allocating its own state. The batch fallback path passes its existing handle through.

Alternative: if the borrow lifetime is awkward, expose a `EngineState::clone_caches()` method that copies the HEAD-blob cache into a fresh state used by the retry. Pure performance optimization; correctness is unchanged.

### Constraints

- Fresh per-attempt state must still pick up new HEAD if HEAD itself moved between attempts. A blob cache keyed on `(head_sha, path)` is safe to reuse only if the HEAD captured at batch start is still HEAD at retry; check `state.head_sha()` matches before reuse, fall back to fresh state otherwise.

### Tests

- Add a test that exercises the batch path with two meshes where one hits a simulated CAS conflict on the first attempt. Assert both meshes complete in the same outcome shape as the no-conflict path.

## Item 4: Compute candidate set once in `run_advice_flush`'s per-touch emission loop

### Current Issue

The flush loop in `packages/git-mesh/src/cli/advice/mod.rs:318-371` walks every touch and for each touch walks every candidate mesh:

```rust
for t in &touches {
    if matches!(t.kind, TouchKind::Added | TouchKind::Deleted) { continue; }
    let action = Action::WholeFile { path: t.path.clone() };
    for mesh in &meshes {
        if emitted_meshes_this_call.contains(&mesh.name) { continue; }
        let Some(active) = mesh.anchors.iter().find(|a| edit_overlaps(&action, a)) else { continue; };
        ...
    }
}
```

`meshes` is already deduped by path-index candidate scoping, so most pairs are positive. The inner `mesh.anchors.iter().find(...)` rescans the anchor list per touch even though most anchors are wholly unrelated to the touched path. For a flush that touches a handful of paths in a single tool call, the work is fine; for a Bash run that mass-edits many files in one tool_use_id, this becomes the dominant cost.

### Recommendation

Build a `HashMap<&str, Vec<&AnchorResolved>>` keyed by anchor path for each candidate mesh once, before the touch loop. The inner loop becomes a single map lookup keyed on `t.path`, then an overlap predicate over only the path-matching anchors.

### Tests

- Existing `tests/advice_path_index_candidates.rs` covers correctness.
- Optional perf test: assert that flushing N modified paths in one call does not scale linearly with the count of unrelated anchors in candidate meshes. Skip if perf instrumentation is too noisy in CI.

## Item 5: Roll up v1 plan and consolidate

### Current Issue

`docs/remaining-implementation-items-plan.md` describes Items 1–8 as future work. They have all shipped; the file now reads as a stale plan. Anyone reading the docs has to cross-reference commits 06d54fc / 30226b5 to know which items are live.

### Recommendation

Replace the v1 file with a one-paragraph note ("Items 1–8 shipped in commits 06d54fc and 30226b5; see this v2 plan for follow-ups."), or delete it outright and let v2 stand as the single living plan. Linkbacks from CARD.md or other docs that reference it should be updated to point at v2.

## Priority Order

1. Item 1 (delete dead code) — biggest readability win, smallest risk, single PR.
2. Item 5 (doc rollup) — trivially small, removes confusion.
3. Item 2 (HashSet dedupe) — small win, low risk, isolated change.
4. Item 4 (precompute path → anchors map) — modest win for big-edit flushes.
5. Item 3 (carry shared state into retry) — only matters under contention; lower priority.

## Validation Plan

For each change under `packages/git-mesh`:

```sh
cd packages/git-mesh
yarn lint
yarn typecheck
yarn test
cd ../..
yarn validate
```

Run focused tests for the touched module first (e.g. `cargo test --test cli_compact` or `cargo test --test advice_path_index_candidates`) before the full suite.
