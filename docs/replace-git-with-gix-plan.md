# Replace `git` subprocess calls with `gix`

## Goal

Eliminate every `Command::new("git")` call in `packages/git-mesh` except
the fetch/push path in `src/sync.rs`, which must remain on the subprocess
because `gix` 0.81 cannot cover credential-helper + SSH-agent auth
across all transports.

After this work, the docstring at `src/sync.rs:8` — "the sole remaining
`Command::new("git")` call site in the production path" — becomes true
again.

## Scope

In-scope: the 12 `git` subprocess call sites listed below, across 8
files in `packages/git-mesh/src/`.

Out of scope:
- `src/sync.rs` fetch/push (intentional; SSH-agent parity).
- `src/resolver/layers/filter_process.rs:56,72` — spawns `git-lfs` and
  `sh` as configured filter drivers; not a `git` call.
- `src/cli/commit.rs:214` — spawns `sh -c` for the editor flow; not a
  `git` call.

## Constraints

- gix version is pinned at `0.81.0` with features `revision`, `status`,
  `sha1`, `blame`, `blob-diff` (see `packages/git-mesh/Cargo.toml`). All
  APIs referenced below are available under that feature set.
- Greenfield: no fallbacks, no dual-path shims. Each slice removes the
  subprocess entirely.
- Golden rule: lint, typecheck, and run all tests after each slice from
  the package directory; final `yarn validate` from the workspace root.

## Call-site inventory

| # | File:line | Command | Replacement |
|---|-----------|---------|-------------|
| 1 | `src/range.rs:100` | `git ls-tree <sha> -- <path>` | `rev_parse_single` → `peel_to_tree` → `lookup_entry_by_path` |
| 2 | `src/staging.rs:577` | `git ls-tree <sha> -- <path>` | same |
| 3 | `src/mesh/commit.rs:351` | `git ls-tree <sha> -- <path>` | same |
| 4 | `src/resolver/engine/whole_file.rs:153` | `git ls-tree <sha> -- <path>` | same |
| 5 | `src/types.rs:721` | `git ls-files --stage` | `Repository::index_or_load_from_head()` → `index::State::entries()` |
| 6 | `src/resolver/engine/whole_file.rs:172` | `git ls-files --stage` | same |
| 7 | `src/resolver/layers/diff.rs:224` | `git ls-files -u -z` | index API, filter `entry.stage() != 0` |
| 8 | `src/resolver/engine/whole_file.rs:190` | `git hash-object <file>` | `gix_object::compute_hash(Kind::Blob, bytes)` |
| 9 | `src/resolver/engine/whole_file.rs:204` | `git hash-object --stdin` | same, stdin bytes |
| 10 | `src/types.rs:415` | `git check-attr filter -- <path>` | `Repository::attributes(...)` → `Platform::at_entry(...).matching_attributes(...)` |
| 11 | `src/types.rs:756` | `git check-attr binary -- <path>` | same attributes API, seed outcome with `"binary"` |
| 12 | `src/types.rs:829` | `git config --get-regexp ^filter\.` | `config_snapshot().sections_by_name("filter")` (or plumbing `File::sections_and_ids()`) |
| 13 | `src/resolver/layers/filter_process.rs:201` | `git config --get <key>` | `config_snapshot().string(...)` |
| 14 | `src/cli/pre_commit.rs:182` | `git diff --cached --name-only -z` | `gix::diff::index(head_tree, index, ...)` |
| 15 | `src/resolver/layers/diff.rs:76` | `git diff` (generic) | `gix::diff::{tree, index, blob}` |
| 16 | `src/resolver/engine/whole_file.rs:129` | `git diff` (generic) | same |
| 17 | `src/resolver/attribution.rs:31` | `git` (attribution lookup — verify exact args) | likely `gix::blame` (feature enabled) |

Seventeen line hits, twelve distinct logical operations.

## Intentional exclusions

- `src/sync.rs:70` — `git fetch` / `git push`. Keep as-is. Update the
  docstring if it drifts during this work.

## Suggested slice order

Order chosen to land small, independently verifiable slices and to
front-load the shared helpers.

1. **Shared `gix` helpers.** Add a thin internal module (e.g.
   `src/git/gix_ext.rs` or extend `src/git.rs`) with:
   - `tree_entry_at(repo, commit, path) -> Option<(mode, oid)>`
   - `index_entries(repo) -> impl Iterator<Item = &Entry>`
   - `hash_blob(bytes) -> ObjectId`
   - `attr_for(repo, path, name) -> Option<BString>`
   - `config_string(repo, key) -> Option<String>`
   Add unit tests against a temp repo fixture.
2. **`ls-tree` sites** (#1–#4). Swap the four call sites to
   `tree_entry_at`. One slice, four files.
3. **`ls-files --stage` + unmerged** (#5, #6, #7). All go through the
   index API. One slice.
4. **`hash-object`** (#8, #9). Replace with `compute_hash`. One slice.
5. **`check-attr`** (#10, #11). Build the `worktree::stack::Platform`
   once per call site; share the outcome seeding helper.
6. **`git config`** (#12, #13). Two call sites, two readers.
7. **`git diff --cached`** (#14). Use `gix::diff::index`.
8. **Generic `git diff`** (#15, #16). Use the appropriate
   `gix::diff::{tree, index, blob}` variant per call site — confirm the
   exact semantics before replacing (these are the least mechanical
   swaps).
9. **`attribution.rs`** (#17). Inspect the exact `git` args first; pick
   between `gix::blame`, `gix::revision`, or a targeted log walk.
10. **Docstring fixup.** Re-verify and, if needed, restore the
    "sole remaining `Command::new("git")` call site" claim in
    `src/sync.rs:8`.

Each slice:
- Removes the subprocess entirely — no feature flag, no fallback.
- Keeps public API stable where the call site is internal; adjust
  return types only when gix's shape is strictly better.
- Ships with updated unit tests in the touched module and runs
  `yarn lint && yarn typecheck && yarn test` from
  `packages/git-mesh/`.
- Final slice runs `yarn validate` at the workspace root.

## Risks

- **Attribute macro resolution.** `git check-attr binary` resolves the
  built-in `binary` macro. Confirm `gix_attributes` expands macros in
  the `Outcome` path; if not, seed both `binary` and its constituent
  attributes (`-text -diff`).
- **Index-vs-HEAD diff parity.** `git diff --cached --name-only -z`
  respects `core.quotepath`, pathspecs, and renames-by-default-off.
  The `gix::diff::index` caller must match those defaults or the
  pre-commit hook behavior drifts.
- **Config semantics for `filter.*`.** `--get-regexp` returns every
  matching key, including multi-valued keys. Make sure the gix
  enumeration walks all sections and subsections, not just the first.
- **`whole_file.rs` generic diff (#16).** The args at line 129 need
  inspection; if it is a merge-base or three-way diff, the replacement
  is `gix::merge` territory, which is more involved than a plain
  `blob-diff`.
- **Performance.** Index loads are cheap once per call, but slice 3
  should load the index once and reuse it if the surrounding code path
  already has a handle.

## gix 0.81 API references

Per-operation documentation links, pinned to the installed version.

**Tree / objects**
- `Repository::rev_parse_single` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.rev_parse_single
- `Repository::write_blob_stream` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.write_blob_stream
- `gix_object::compute_hash` (hash-object without writing) — https://docs.rs/gix-object/0.58.0/gix_object/fn.compute_hash.html
- `gix_object` module (re-exported as `gix::objs`) — https://docs.rs/gix-object/0.58.0/gix_object/index.html

**Index**
- `Repository::index_or_load_from_head` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.index_or_load_from_head
- `gix::index` module (entries, stages, flags) — https://docs.rs/gix/0.81.0/gix/index/index.html

**Attributes / worktree stack**
- `Repository::attributes` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.attributes
- `gix::worktree::stack` — https://docs.rs/gix/0.81.0/gix/worktree/stack/index.html

**Config**
- `Repository::config_snapshot` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.config_snapshot
- `gix::config` module — https://docs.rs/gix/0.81.0/gix/config/index.html

**Diff**
- `Repository::diff` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.diff
- `gix::diff` — https://docs.rs/gix/0.81.0/gix/diff/index.html
- `gix::diff::index` — https://docs.rs/gix/0.81.0/gix/diff/index/index.html
- `gix::diff::tree` — https://docs.rs/gix/0.81.0/gix/diff/tree/index.html
- `gix::diff::blob` — https://docs.rs/gix/0.81.0/gix/diff/blob/index.html

**Status / attribution**
- `Repository::status` — https://docs.rs/gix/0.81.0/gix/struct.Repository.html#method.status
- `gix::revision` — https://docs.rs/gix/0.81.0/gix/revision/index.html
- `gix_blame` (for `resolver/attribution.rs` replacement) — https://docs.rs/gix-blame/0.11.0/gix_blame/index.html

## Validation gates

- Per slice: `yarn lint && yarn typecheck && yarn test` in
  `packages/git-mesh/`, focused test runs where possible.
- Final: `yarn validate` at the workspace root (exit code 0).
- Spot-check: `grep -rn 'Command::new("git")' packages/git-mesh/src`
  should return exactly the `sync.rs` hit on completion.
