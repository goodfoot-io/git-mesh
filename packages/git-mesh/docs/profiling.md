# Profiling `git mesh stale`

Two complementary tools cover the common perf questions:

1. **`perf record` + `inferno-flamegraph`** — self-time profile. "Which functions are hot?"
2. **`git mesh stale --perf-trace <path>`** — per-anchor wall-clock distribution. "Is the runtime uniform across many anchors, or concentrated in a few?"

Both are pre-installed in the devcontainer image
([`./.devcontainer/Dockerfile`](./.devcontainer/Dockerfile)) — no `apt install` or
`cargo install` step is needed on a fresh rebuild.

## Flame graph capture

```bash
perf record -F 199 -g --call-graph=dwarf \
    -o /tmp/stale.perf \
    -- git mesh stale --perf
perf script -i /tmp/stale.perf 2>/dev/null \
    | inferno-collapse-perf \
    | inferno-flamegraph > /tmp/stale.svg
```

`-F 199` samples at 199 Hz (prime, to avoid lock-step with periodic events). `--call-graph=dwarf` walks DWARF unwind info so Rust frames resolve cleanly without frame-pointer instrumentation.

### `perf_event_paranoid` fallback

Some host kernels lock `perf` event capture to root. If `perf record` reports `Permission denied` or `Access to performance monitoring and observability operations is limited`, lower the paranoia level from the container:

```bash
sudo sysctl kernel.perf_event_paranoid=1
```

The devcontainer image does not set this at startup — many hardened hosts reject the sysctl, and a failing image bootstrap is worse than a documented one-line fix.

## Per-anchor wall-clock trace (`--perf-trace`)

```bash
git mesh stale --perf-trace /tmp/trace.csv
```

Writes one CSV row per resolved anchor to `/tmp/trace.csv`. The flag is opt-in; without it, no per-anchor instrumentation runs.

### CSV schema

| Column | Type | Description |
|--------|------|-------------|
| `mesh` | string | Mesh name (e.g. `wiki/architecture/refs`) |
| `anchor_id` | string | Anchor identifier within the mesh |
| `anchor_sha` | hex | Commit SHA the anchor is pinned to |
| `path` | string | File path the anchor references |
| `wall_us` | u128 | Wall-clock microseconds spent in [`resolve_anchor_inner`](../src/resolver/engine/anchor.rs) |
| `fast_path` | bool | `true` if the anchor returned via [`clean_head_fast_path`](../src/resolver/engine/anchor.rs) |
| `status` | enum | One of `Fresh`, `Moved`, `Changed`, `Orphaned`, `MergeConflict`, `Submodule`, `ContentUnavailable` |

Values containing `,`, `"`, or newline are RFC-4180-escaped (wrapped in `"`, internal `"` doubled).

**Column order is a stable interface.** External tooling that consumes the CSV may pin to it; changes go through a deprecation cycle.

### Usage constraints

`--perf-trace` requires a full scan and rejects positional paths — partial scans defeat the "where did the time go?" question the flag exists to answer:

```bash
git mesh stale --perf-trace /tmp/trace.csv          # OK: full scan
git mesh stale --perf-trace /tmp/trace.csv some/path  # CliError
```

It also conflicts with `--compact` (a mutation mode, out of scope for tracing).

### Quick analyses

```bash
# Top 10 slowest anchors
awk -F, 'NR>1 {print $5"\t"$2"\t"$4}' /tmp/trace.csv | sort -rn | head

# Mean wall-clock, count
awk -F, 'NR>1 {sum += $5; count++} END {print sum/count " us avg, " count " anchors"}' /tmp/trace.csv

# Fast-path vs full-resolution split
awk -F, 'NR>1 {print $6}' /tmp/trace.csv | sort | uniq -c

# Anchors > 50 ms
awk -F, 'NR>1 && $5 > 50000 {print $5"\t"$1"\t"$2}' /tmp/trace.csv | sort -rn
```

## Why two tools

`perf` samples at 199 Hz; many anchors complete in well under 5 ms and fall below the sample resolution, so flame graphs answer "which functions are hot" but not "which anchors are slow." The CSV emitter measures the bounded interval (function entry to return) and is the right tool for per-anchor distribution analysis. The two are complementary — use both when triaging a regression.

## Trace overhead

When `--perf-trace` is absent, the resolver does not capture per-anchor traces; the session's trace buffer stays `None` and the per-anchor loop pays a single `Option::is_some()` check per iteration. When the flag is set, each anchor adds one `Instant::now()` (already captured for the existing `per_anchor_us` summary), a snapshot of the fast-path counter, and a `Vec::push` of ~80 bytes — well under 1 ms total on a 2,600-anchor workspace.
