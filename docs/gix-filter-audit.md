# gix Filter-Pipeline Audit (Phase 0 / Slice 1)

Scope: confirm what the `gix` 0.81 filter pipeline covers for the
"core filters" group (D3 in `docs/stale-layers-plan.md`), record the
APIs we wired up in slice 1, and call out what's intentionally
deferred.

## APIs used

`ContentRef::read_normalized` (in `packages/git-mesh/src/types.rs`)
dispatches per variant:

- **`ContentRef::Blob(oid)`** — raw blob via `Repository::find_object` →
  `into_blob().detach().data`. HEAD-stored blobs are already in
  canonical (to-git) form, so no filter is invoked. This is the only
  variant exercised by the slice-1 HEAD-only fast path.
- **`ContentRef::WorktreeFile(path)`** — `Repository::filter_pipeline(None)`
  returns a `gix::filter::Pipeline` plus the index it was primed with.
  We feed the worktree file through `Pipeline::convert_to_git`, which
  internally consults `gix_filter`'s attribute stack and applies the
  to-git half of every configured driver. Symlinks short-circuit to the
  link target string (filters do not run on symlinks; matches git's
  behavior and plan §D2).
- **`ContentRef::Sidecar(path)`** — raw `std::fs::read`. Re-normalization
  across `.gitattributes` changes (the freshness-stamp dance described
  in plan §B2) is a later slice.

## Coverage — core filter set

`gix_filter::Pipeline` (driven through `gix::filter::Pipeline::convert_to_git`
and `convert_to_worktree`) is the supported in-tree implementation of:

- `core.autocrlf` (`true` / `input` / `false`) — read from
  `repo.config` in `gix::filter::Pipeline::options`.
- `core.eol` (`crlf` / `lf` / `native`) — same path.
- `text` / `text=auto` and `eol=crlf|lf` per-path attributes —
  resolved via `gix_filter`'s attribute stack against the worktree
  cache built by `Repository::attributes_only`.
- `ident` keyword expansion / contraction — handled inside
  `gix_filter` once the attribute is set.
- `working-tree-encoding` round-trip with `core.checkRoundtripEncoding` /
  `core.safecrlf` — wired through `Pipeline::options`.

This matches the plan's "core filters" group. No targeted subprocess
fallback is required for the slice-1 acceptance test.

## Deferred

The following are **not** implemented in slice 1. The reader returns
`Error::Git("filter X not implemented in this slice")` if it ever has
to invoke them; the engine slice will surface those as
`RangeStatus::ContentUnavailable(FilterFailed { filter })`.

- **`filter=lfs`** — `gix` has no LFS support. Plan calls for a managed
  `git-lfs filter-process` subprocess (lazy, reused across a `stale`
  run, `GIT_LFS_SKIP_SMUDGE=1`). Detection is by `.gitattributes`
  attribute, not blob sniffing.
- **Custom `filter=<name>` drivers** (`filter.<name>.process` or
  smudge/clean shell pairs) — plan calls for one managed
  `git filter-process`-protocol subprocess per driver, also lazy. The
  current `convert_to_git` call will pass these through to whatever
  `gix_filter` does by default; that is *not* the same thing as the
  spec'd long-lived process orchestrator, so the readers slice will
  intercept these before they reach `gix_filter`.
- **Sidecar re-normalization on read.** Sidecars carry a
  `.gitattributes` SHA + filter-driver-list hash. On mismatch the
  engine must re-normalize both sides before comparing rather than
  trusting stored bytes (plan §B2). Slice 1 reads sidecars raw.
- **Concurrency guard** (index-file SHA-1 trailer at start/end of run)
  — engine slice.

## Honest gaps

- I did not write a fixture comparing `convert_to_git` output against
  `git cat-file --filters` byte-for-byte. The plan's Phase 0 acceptance
  asks for that. Slice 1's acceptance test (HEAD-only, `--no-worktree
  --no-index --no-staged-mesh`) doesn't reach the worktree reader, so
  the fixture comparison remains a pre-requisite for the readers slice
  rather than a slice-1 blocker.
- Slice 1's `WorktreeFile` reader will silently use the default
  `gix_filter` handling for `filter=<name>` drivers — for slice-1 tests
  that never enable the worktree layer this is unreachable, but a
  follow-up slice must intercept before `convert_to_git` to honor the
  managed-subprocess design.
