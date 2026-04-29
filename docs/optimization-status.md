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
- `profiling/bench-mesh-scale.smoke.md` and `.csv`: synthetic 100-mesh mixed-anchor smoke across broad operations and wiki-style filtered `ls`.
- `profiling/bench-mesh-scale.1000x2.md` and `.csv`: synthetic 1,000-mesh, 2-anchor mixed workload.
- `profiling/git-mesh-profile.1000x2.*.err` and `.out`: direct `GIT_MESH_PERF=1` logs for 1,000-mesh `ls`, filtered `ls`, and `stale`.

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
- Added a scale benchmark harness, `scripts/bench-mesh-scale.sh`, for synthetic 100/1,000/10,000 mesh sweeps with 2/3/4/5 primary anchors, optional 10/20/120 edge anchors, line/whole/mixed anchor distributions, loose-ref/packed-ref/maintenance variants, and process-per-query wiki `ls <path>#L<start>-L<end> --porcelain` timing.
- Avoided reading mesh config blobs in `git mesh ls`, which only needs the mesh why text and anchor ids.
- Replaced repeated linear staged/committed name checks in `git mesh ls` with command-local `HashSet` membership.
- Removed a redundant post-render mesh commit info lookup from default `git mesh show <name>`.

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

Continued scale loop synthetic mixed-anchor results:

| meshes | anchors/mesh | add | commit | show | ls --porcelain | filtered ls hit | filtered ls miss | stale | pre-commit | wiki hit workload | wiki miss workload |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | 2 | 0.0139s | 0.0176s | 0.0043s | 0.0170s | 0.0211s | 0.0109s | 0.1737s | 0.6528s | 5 queries / 0.0615s | 5 queries / 0.0552s |
| 100 | 3 | 0.0507s | 0.0531s | 0.0049s | 0.0353s | 0.0298s | 0.0144s | 0.1424s | 0.6925s | 5 queries / 0.0673s | 5 queries / 0.0680s |
| 1,000 | 2 | 0.0199s | 0.1651s | 0.0037s | 0.0778s | 0.0915s | 0.1087s | 1.4105s | 7.7792s | 3 queries / 0.2570s | 3 queries / 0.2245s |

The synthetic scale harness intentionally uses a process-per-query wiki workload because the downstream wiki CLI currently shells out once per fragment link. The 1,000-mesh fixture used mixed anchors: one whole-file wiki anchor plus one code line-range anchor per mesh.

Direct `GIT_MESH_PERF=1` on the kept 1,000-mesh fixture:

| operation | total | notable spans |
|---|---:|---|
| `ls --porcelain` | 91.221 ms | list mesh refs 7.849 ms; read committed mesh/anchor records 81.376 ms; sort/page/render 1.817 ms |
| `ls src/module_001.rs#L15-L19 --porcelain` | 87.477 ms | list mesh refs 6.074 ms; read committed mesh/anchor records 81.101 ms; path filter 0.069 ms |
| `stale --no-exit-code` | 2,576.988 ms | list meshes 6.287 ms; init layers 8.685 ms; resolve all meshes 2,570.378 ms; render human 6.310 ms |

Top bottlenecks found:

- Filtered `ls` is still O(all committed mesh/anchor records). The path/range filter is cheap once records are materialized; the remaining bottleneck is the current ref/object layout and lack of an authoritative path-oriented index.
- Workspace-wide `stale` at 1,000 meshes is dominated by repeated anchor resolution, not mesh ref enumeration or rendering.
- `pre-commit` is substantially slower than plain `stale` on the synthetic fixture and needs a separate hook-path profile before changing behavior.
- `show <name>` remains effectively O(anchors in that mesh) at this scale.

Accepted experiments:

- Keep the current storage model for this pass and continue evolutionary read-path fixes.
- Add a synthetic benchmark harness before attempting a schema change.
- Use command-local data structures only; no persistent cache or sidecar cache state was added.

Rejected or deferred experiments:

- Durable path/range index: likely needed for 10,000-mesh wiki workloads, but deferred until the new harness completes 10,000-mesh, packed-ref, and maintenance variants. If added, it should be authoritative Git-backed mesh metadata, not a derived cache.
- Reftable-specific path: deferred because local fixture coverage and Git support detection still need to be added to the harness.
- Automatic Git maintenance during normal CLI operations: deferred to avoid surprising repository mutation. The harness can measure `--pack-refs` and `--maintenance` variants explicitly.

## Validation

Completed:

- `yarn lint` in `packages/git-mesh`
- `yarn typecheck` in `packages/git-mesh`
- `yarn test` in `packages/git-mesh`: 647 passed, 48 skipped
- `bash -n scripts/bench-mesh-scale.sh`
- `cargo fmt --check` in `packages/git-mesh`
- `yarn validate` from the workspace root: 647 Rust tests passed, hook tests passed, release build and VSIX packaging passed
