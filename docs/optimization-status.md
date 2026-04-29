# git-mesh Optimization Status

Date: 2026-04-29

## Scope

This round focused on `packages/git-mesh`, especially `git mesh stale` on large repositories. The target repository for external benchmarks was `vercel/next.js`, using `scripts/bench-mesh.sh` from the workspace root.

No persistent cache was added. The new reuse points are per-command state only and are dropped when the CLI invocation exits.

## Profiling Tooling

- `scripts/bench-mesh.sh`: benchmark harness that clones or updates a bare Next.js cache, creates scratch worktrees, seeds meshes, and records operation latency to Markdown and CSV.
- `GIT_MESH_PERF=1` and `git mesh --perf`: opt-in internal timing logs emitted to stderr as `git-mesh perf: <operation> <ms> ms`.
- Kept benchmark fixtures with `scripts/bench-mesh.sh --keep` for repeatable local profiling against the same seeded repo.
- Direct dirty-fixture profiling with commands like:

```sh
GIT_MESH_PERF=1 git -C /tmp/git-mesh-bench-h0tfrp/v9.0.0 mesh stale --no-exit-code
```

## Benchmark Artifacts

Important benchmark outputs preserved locally under ignored `profiling/`:

- `profiling/bench-mesh.v31.md`: previous v1.0.31 baseline.
- `profiling/bench-mesh.v32.cold.md`: previous v1.0.32 cold baseline.
- `profiling/bench-mesh.v33.cycle5.md` and `.csv`: latest v9 optimization cycle.
- `profiling/bench-mesh.v33.repo-size2.md` and `.csv`: latest repo-size sanity check.
- `profiling/git-mesh-profile.finaldirty.err` and `.out`: latest dirty-fixture perf log and output.

## Optimizations Applied

- Added opt-in performance logging behind `--perf` and `GIT_MESH_PERF=1`.
- Reused one `EngineState` across workspace-wide `stale` resolution instead of recreating layer state per mesh.
- Added per-command commit reachability reuse and a `HEAD == anchor` reachability fast path.
- Shared grouped history walks in `ResolveSession` by `(anchor_sha, copy_detection)`.
- Added a clean tracked-layer status probe so clean index/worktree paths skip full structured diff initialization.
- Added a dirty-path targeted worktree diff path. `git status --porcelain=v1 -z -uno` provides exact tracked worktree path hints for simple dirty states; rename, copy, malformed, or unmerged cases fail closed to existing full scans.
- Added fresh-HEAD fast paths for line-range and whole-file anchors when content layers match `HEAD`.
- Removed repeated repository opens from read paths by using the already-open `gix::Repository` for anchor and mesh reads.
- Added per-command `HEAD:path` blob lookup reuse for repeated anchor paths.

## Current Results

Latest v9 Next.js matrix: `profiling/bench-mesh.v33.cycle5.md`

| meshes | anchors/mesh | v31 stale median | latest stale median | improvement |
|---:|---:|---:|---:|---:|
| 1 | 2 | 4.3335s | 0.0180s | 241x |
| 1 | 10 | 50.4538s | 0.0141s | 3577x |
| 10 | 2 | 107.9532s | 0.0183s | 5890x |
| 10 | 10 | not completed in v31 | 0.0171s | n/a |

Latest repo-size sanity check: `profiling/bench-mesh.v33.repo-size2.md`

| ref | meshes | anchors/mesh | stale |
|---|---:|---:|---:|
| v9.0.0 | 1 | 2 | 0.0408s |
| v13.0.0 | 1 | 2 | 0.0349s |
| canary | 1 | 2 | 0.0548s |

Dirty-worktree profiling on a kept v9 fixture improved from about 502 ms before targeted layer initialization to about 14 ms after the latest read-path changes, with byte-identical output.

## Validation

Completed:

- `yarn lint` in `packages/git-mesh`
- `yarn typecheck` in `packages/git-mesh`
- `yarn test` in `packages/git-mesh`: 647 passed, 48 skipped
- `yarn validate` from the workspace root: 647 Rust tests passed, hook tests passed, release build and VSIX packaging passed
