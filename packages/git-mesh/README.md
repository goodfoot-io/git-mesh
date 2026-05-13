# git mesh

`git mesh` tracks implicit semantic dependencies in a git repository — line-range or whole-file anchors that participate in a coupling no schema, type, or test enforces. Each mesh names its anchors, carries a `why` sentence defining the subsystem they collectively form, and surfaces drift via `git mesh stale` when those anchors diverge from their anchored state.

The primary CLI surface lives in `src/cli/mod.rs`. Run `git mesh --help` or `git mesh stale --help` for flag reference.

### Upgrading from the sqlite cache

The trail cache now lives at `<common_dir>/mesh/cache/v1/` as a BLAKE3-keyed content-addressed filesystem store. The previous sqlite-backed cache at `<common_dir>/mesh/cache/mesh_cache.sqlite` (or `<git_dir>/mesh/cache/mesh_cache.sqlite` on older per-worktree installations), along with its `-shm` and `-wal` companions, is unused and can be removed with a single `rm -f <common_dir>/mesh/cache/mesh_cache.sqlite*` (and the `<git_dir>` variant if present). Nothing reads those files anymore; `git mesh doctor --gc-trail-cache` operates only on the new store.

## Profiling

Perf investigation tooling is documented in [`./docs/profiling.md`](./docs/profiling.md):

- **Flame graph capture** — `perf record` + `inferno-flamegraph` recipe for identifying hot functions.
- **`--perf-trace <path>`** — opt-in per-anchor wall-clock CSV emitter for `git mesh stale`; CSV schema, usage constraints, and quick analysis snippets are documented there.
