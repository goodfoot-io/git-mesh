# `CONTENT_UNAVAILABLE`

`CONTENT_UNAVAILABLE` means the content should exist but cannot be read locally without a network call. **The resolver never auto-fetches.** Every reason points at a specific fix.

## Reasons

- **`LfsNotFetched`** — LFS pointer resolves, real bytes not in local cache.
  Fix: `git lfs fetch`, or `--ignore-unavailable`.
- **`LfsNotInstalled`** — No `git-lfs` binary on PATH.
  Fix: install `git-lfs`, or `--ignore-unavailable`.
- **`PromisorMissing`** — Partial clone; blob not fetched from the promisor remote.
  Fix: `git fetch` with an unfiltered spec, or `--ignore-unavailable`.
- **`SparseExcluded`** — Sparse-checkout excludes the path.
  Fix: adjust the sparse cone, or `--ignore-unavailable`.
- **`FilterFailed`** — A custom smudge/clean filter returned non-zero.
  Fix: repair the filter driver; re-run.
- **`IoError`** — Local read failed for reasons unrelated to the above (permissions, missing file, etc.).
  Fix: investigate the specific error the resolver printed.

## `--ignore-unavailable`

Downgrades only `CONTENT_UNAVAILABLE` findings so they print without failing exit code. Real drift findings (`CHANGED`, `MOVED`, terminal statuses other than `CONTENT_UNAVAILABLE`) still fail as usual.

```bash
git mesh stale --ignore-unavailable
```

Appropriate use:
- Fresh CI clones where `git lfs fetch` has not run yet.
- Partial clones where a blob might not be materialized.
- Advisory reports where unavailable content should be counted but not block.

Inappropriate use:
- Silencing a finding you don't want to debug. Fix the fetch/install/sparse/filter problem instead.

## Why no auto-fetch

The resolver is intentionally local and predictable. A stale run should not reach across the network, start LFS downloads, or change sparse state as a side effect. If content is unavailable, the tool says so and lets the caller decide whether to fetch, adjust sparse config, or accept the gap.
