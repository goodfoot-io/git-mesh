# git mesh

`git mesh` tracks implicit semantic dependencies in a git repository — line-range or whole-file anchors that participate in a coupling no schema, type, or test enforces. Each mesh names its anchors, carries a `why` sentence defining the subsystem they collectively form, and surfaces drift via `git mesh stale` when those anchors diverge from their anchored state.

The primary CLI surface lives in `src/cli/mod.rs`. Run `git mesh --help` or `git mesh stale --help` for flag reference.

## Profiling

Perf investigation tooling is documented in [`./docs/profiling.md`](./docs/profiling.md):

- **Flame graph capture** — `perf record` + `inferno-flamegraph` recipe for identifying hot functions.
- **`--perf-trace <path>`** — opt-in per-anchor wall-clock CSV emitter for `git mesh stale`; CSV schema, usage constraints, and quick analysis snippets are documented there.
