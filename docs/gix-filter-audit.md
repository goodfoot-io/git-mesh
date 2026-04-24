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

## Fail-loud short-circuit (corrective slice after slice 2)

The slice-2-bundled `WorktreeFile` reader and the engine's index/HEAD
blob read sites originally fell through to `gix_filter`'s default
driver dispatch silently. The corrective slice intercepts before the
filter pipeline runs:

- A new `Error::FilterFailed { filter }` variant is the typed
  short-circuit signal raised from `types::ContentRef::read_normalized`
  (`WorktreeFile` arm) and from `stale::read_worktree_normalized`. The
  engine catches it at the per-layer read site and surfaces
  `RangeStatus::ContentUnavailable(UnavailableReason::FilterFailed
  { filter })`. We chose a typed `Error` variant (rather than a
  `Result<Bytes, FilterShortCircuit>` shape) because the existing
  reader signature is already `Result<Vec<u8>>` and the engine site
  was the only caller — adding a variant kept the plumbing tight.
- Probe is `git check-attr filter -- <path>`, run from the worktree.
  Any non-`unspecified` / non-`unset` / non-`set` value short-circuits.

### Allowlist

The `filter` `.gitattributes` attribute is reserved for
`filter=<name>` driver dispatch. Core normalization (`text`,
`text=auto`, `eol`, `ident`, `working-tree-encoding`, `core.autocrlf`,
`core.eol`) is driven via *separate* attributes / config values that
never set the `filter` attribute. As a result the slice-2 allowlist
for the `filter` attribute itself was intentionally **empty**: any
explicit `filter=<name>` resolved to a non-core driver (LFS, custom
process filter, git-crypt, …) and short-circuited. See
`types::is_core_filter`.

**Slice 6** added `lfs` to the allowlist. `filter=lfs` paths are now
routed through a managed `git-lfs filter-process` subprocess (lazy,
reused across a `stale` run, `GIT_LFS_SKIP_SMUDGE=1` in env). Pointer
OIDs are compared first as a fast-path; cache misses surface as
`ContentUnavailable(LfsNotFetched)`; spawn failures surface as
`ContentUnavailable(LfsNotInstalled)`. See `stale::resolve_lfs_range`
and the LFS subprocess block at the bottom of `stale.rs`.

When slice 7 (custom filter-process) lands the allowlist will widen
again to admit any configured `filter.<name>.process` driver via the
same orchestrator pattern.

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

- **Outstanding (not closed by this slice).** A byte-identical fixture
  comparing `convert_to_git` output against `git cat-file --filters`
  for each core filter (`core.autocrlf=true|input`, `text=auto`,
  `eol=crlf|lf`, `ident`, `working-tree-encoding`) is still owed.
  The CRLF acceptance test
  (`crlf_checkout_of_lf_blob_no_false_drift`) exercises one slice of
  this and passes, but it is not the byte-for-byte fixture the Phase 0
  plan calls for. Tracked as debt for a follow-up; the corrective
  slice did not land it.
- ~~Slice 1's `WorktreeFile` reader will silently use the default
  `gix_filter` handling for `filter=<name>` drivers~~ — closed by the
  corrective slice's fail-loud short-circuit (see above).
