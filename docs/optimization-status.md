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
- `profiling/bench-mesh-scale.after.100x2.md` and `.csv`: post-pass 100-mesh, 2-anchor mixed workload with process-per-query wiki samples.
- `profiling/git-mesh-profile.after.1000x2.*.err` and `.out`: post-pass direct `GIT_MESH_PERF=1` logs for 1,000-mesh `ls`, filtered `ls`, `stale`, and `pre-commit`.

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
- Added a mesh-ref enumeration path that carries each mesh ref's target OID into bulk reads. `ls`, workspace-wide `stale`, and `pre-commit` no longer parse the same mesh revision once during listing and again during the mesh read.
- Made `git mesh ls --porcelain` skip committed mesh why text and staged-state reads unless `--search` or human rendering needs them.
- Reworked `git mesh pre-commit` to resolve committed meshes through one shared resolver state, matching the workspace-wide `stale` reuse pattern while preserving staging-only mesh reporting.

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

Post-pass direct `GIT_MESH_PERF=1` on a fresh 1,000-mesh, 2-anchor mixed fixture:

| operation | total | notable spans |
|---|---:|---|
| `ls --porcelain` | 91.656 ms | list mesh refs 19.135 ms; read committed mesh/anchor records 70.977 ms; sort/page/render 1.361 ms |
| `ls src/module_001.rs#L15-L19 --porcelain` | 89.138 ms | list mesh refs 19.131 ms; read committed mesh/anchor records 69.583 ms; path filter 0.220 ms |
| `stale --no-exit-code` | 1,718.633 ms | resolve all meshes 1,718.439 ms; resolve mesh loop 1,695.391 ms; render human 0.001 ms |
| `pre-commit --no-exit-code` | 2,643.159 ms | shared resolve mesh loop 2,576.382 ms; pre-commit resolve wrapper 2,631.430 ms |

Post-pass 100-mesh, 2-anchor mixed harness sample:

| meshes | anchors/mesh | ls --porcelain | filtered ls hit | filtered ls miss | stale | pre-commit | wiki hit workload | wiki miss workload |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | 2 | 0.0210s | 0.0737s | 0.0264s | 0.4145s | 0.4399s | 3 queries / 0.1367s | 3 queries / 0.1470s |

Top bottlenecks found:

- Filtered `ls` is still O(all committed mesh/anchor records). The path/range filter is cheap once records are materialized; the remaining bottleneck is the current ref/object layout and lack of an authoritative path-oriented index.
- Workspace-wide `stale` at 1,000 meshes is dominated by repeated anchor resolution, not mesh ref enumeration or rendering.
- `pre-commit` is substantially slower than plain `stale` on the synthetic fixture and needs a separate hook-path profile before changing behavior.
- `show <name>` remains effectively O(anchors in that mesh) at this scale.
- Mesh-ref re-resolution was a measurable bulk-read cost. Carrying mesh ref OIDs from enumeration into `ls`, `stale`, and `pre-commit` improved the 1,000-mesh `stale` direct run from the prior documented 2.577 s to 1.719 s on the new fixture, and reduced `pre-commit` from the prior 7.779 s harness result to 2.643 s direct perf on the new fixture.

Accepted experiments:

- Keep the current storage model for this pass and continue evolutionary read-path fixes.
- Add a synthetic benchmark harness before attempting a schema change.
- Use command-local data structures only; no persistent cache or sidecar cache state was added.
- Reuse mesh ref target OIDs in bulk operations. This is command-local state derived from authoritative Git refs and avoids repeated revision parsing.
- Share one resolver state across `pre-commit` committed mesh resolution.

Rejected or deferred experiments:

- Durable path/range index: likely needed for 10,000-mesh wiki workloads, but deferred until the new harness completes 10,000-mesh, packed-ref, and maintenance variants. If added, it should be authoritative Git-backed mesh metadata, not a derived cache.
- Reftable-specific path: deferred because local fixture coverage and Git support detection still need to be added to the harness.
- Automatic Git maintenance during normal CLI operations: deferred to avoid surprising repository mutation. The harness can measure `--pack-refs` and `--maintenance` variants explicitly.
- Command-local anchor-ref OID snapshot: rejected for now. On the 1,000-mesh fixture it added a 30-60 ms namespace scan/peel cost to `ls` and regressed `stale` to about 4.7 s because the resolver already benefits from targeted anchor ref reads and object reuse.

## Validation

Completed:

- `yarn lint` in `packages/git-mesh`
- `yarn typecheck` in `packages/git-mesh`
- Focused `yarn test -E 'binary(cli_ls)'`: 26 passed
- Focused `yarn test -E 'binary(pre_commit_hook_integration)'`: 10 passed
- Focused `yarn test -E 'binary(stale_mesh_integration)'`: 35 passed
- `yarn test` in `packages/git-mesh`: 659 passed, 48 skipped
- `cargo fmt --check` in `packages/git-mesh`
- `yarn validate` from the workspace root: 659 Rust tests passed, hook tests passed, release build and VSIX packaging passed
