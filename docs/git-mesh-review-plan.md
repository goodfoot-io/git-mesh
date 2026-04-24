# `git-mesh` review plan — critical bugs and minor quality issues

This plan addresses the concrete implementation defects identified in
`docs/manual-validation-of-git-mesh.md` (the five critical bugs and the
five minor quality issues). Documentation bugs are out of scope here and
will be addressed in a separate handbook-editing pass.

Each item names the exact file(s), the intended fix, the test shape that
will lock the fix down, and rough ordering so validation can pass at
each slice. Package root: `packages/git-mesh/`.

## Slice 1 — LFS line-range commit (Critical #1)

**Symptom.** `git mesh commit` on any staged LFS line-range fails with
`error: invalid range: start=N end=M`.

**Root cause.** `src/mesh/commit.rs:130-138` validates a staged add by
re-reading the anchor commit's raw blob through
`git::path_blob_at` → `git::blob_line_count`. For an LFS-tracked path
that blob is the ~3-line LFS pointer, so `end > line_count` always
trips. The stage-time path (`src/staging.rs:368-413`) reads through the
worktree (filters applied) and already correctly captured the 50-line
content into the sidecar; the commit pipeline simply doesn't use it.

**Fix.**

1. In `src/mesh/commit.rs`, for each staged add with extent
   `RangeExtent::Lines`, use the already-captured sidecar line count
   as the source of truth. The sidecar is read alongside the staging
   records; pipe its `line_count` into the bounds check.
2. When the sidecar is missing (unexpected — every `append_prepared_add`
   writes one), fall back to rendering the filtered content via the
   existing `filter_process`/`lfs` readers in
   `src/resolver/layers/` so validation stays aligned with the
   resolver's later slice reads.
3. Add a sidecar-line-count cache on `PreparedAdd` so we don't re-read
   bytes twice.
4. Drop the `git::path_blob_at` → `blob_line_count` branch for
   line-range validation entirely; it can only be right for paths
   without a content filter, and the filtered path is the superset.
5. Keep the existing whole-file validation at
   `src/mesh/commit.rs:140-152` (it only checks tree existence).

**Test.** Integration test in `tests/` that sets up `git lfs install
--local`, tracks `*.tsv`, commits a 50-line file, stages `#L1-L10`,
runs `git mesh commit`, and asserts exit 0 + mesh ref advances. Add a
second case: `#L1-L200` against a 50-line file should still fail with
`InvalidRange` so we don't regress the real bounds check.

## Slice 2 — Whole-file staged re-anchor ack (Critical #2)

**Symptom.** After a whole-file blob swap + `git add <path>` +
`git mesh add <name> <path>`, `git mesh stale` prints
`(drift: sidecar mismatch)` on the pending op and omits `(ack)` on the
finding. Exit 1. `git mesh commit` succeeds afterward, so the data
model is fine — only the stale/pending path is wrong.

**Root cause.** Two separate misreads of "live bytes" for binary
whole-file pins:

- `src/resolver/engine/pending.rs:164-204` (`pending_add_drift`): when
  the normalization stamp differs between capture and now, `renormalize`
  at line 101-102 runs `String::from_utf8_lossy(bytes).replace("\r\n",
  "\n")`. On a PNG, `\xff` becomes U+FFFD (three UTF-8 bytes), which
  destroys binary equality.
- `src/resolver/engine/pending.rs:183-187`: the `live_norm` side does
  the same lossy + CRLF rewrite unconditionally. Even if the stamp
  matches on the sidecar, live bytes get corrupted, so a byte-identical
  binary re-anchor registers as mismatched.

**Fix.**

1. Gate normalization on the path's text/binary attribute, not on a
   stamp comparison. If `.gitattributes` declares the path binary (or
   it resolves as binary via gix's `check_attrs`), compare raw bytes on
   both sides — no UTF-8 round-trip, no `\r\n` rewrite.
2. For text paths whose stamp matches, return bytes as-is (current
   behavior, still correct).
3. For text paths whose stamp differs, do a byte-safe line-ending
   normalization (`bstr`-level replace of `\r\n` with `\n`) without
   `from_utf8_lossy`. Apply it symmetrically to sidecar and live.
4. Duplicate the same fix into the engine ack loop in
   `src/resolver/engine/pending.rs:30-65` (it calls `renormalize` on
   the sidecar but reads `live_norm` via `read_live_for_range` — verify
   that path does not UTF-8-lossify either; if it does, same treatment).

**Test.** Integration fixture: commit a tiny PNG, mesh-add whole-file,
swap bytes, `git add`, mesh-add again, assert `stale` prints `(ack)`
and exits 0. Second case: same with a symlink whose target string
changed. Third case: text file under a filter whose normalization stamp
has changed mid-session, byte-identical content across CRLF policies,
asserts `(ack)`.

## Slice 3 — Duplicate-range rejection vs last-write-wins (Critical #3)

**Symptom.** `git mesh add m f.txt#L1-L10` followed by the same command
errors `duplicate range location in mesh: f.txt:1-10` instead of the
documented last-write-wins.

**Root cause.** `src/cli/commit.rs:21-51`. Two separate checks:

- Lines 21-30: rejects duplicates within a single `git mesh add`
  invocation.
- Lines 34-51: rejects an in-this-invocation add when the staging
  already contains an un-removed add for the same `(path, extent)`.

Both should flip to "supersede" semantics per the handbook.

**Fix.**

1. Replace the "within-invocation" check (21-30) with coalescing: keep
   the last occurrence, drop earlier duplicates, log nothing. (Silent
   coalescing matches `git add` stage semantics; a warning is optional
   and should not be an error.)
2. Replace the "against-existing-staging" check (34-51) with
   last-write-wins: when a staged add already exists at the same
   `(path, extent)`, rewrite that slot instead of appending. Two
   options:
   - (preferred) Mark the existing op as superseded — e.g. delete the
     old sidecar + meta files and the old line in the staging ops
     file, then append the new one. This keeps
     `.git/mesh/staging/<name>.<N>` numbering dense.
   - Append the new op with a fresh N; have the resolver collapse by
     `(path, extent)` keeping max-N. Matches the existing
     last-write-wins comment at `src/resolver/engine/pending.rs` (line
     11 in the module header, worth confirming).
3. Extend `append_prepared_add` in `src/staging.rs:430-459` to take
   over the existing-op cleanup, so the CLI handler stays minimal.

**Test.** Integration: two sequential `git mesh add m f.txt#L1-L10`
calls with a mid-sequence edit to `f.txt` succeed; sidecar reflects
post-edit bytes; `git mesh commit` produces a mesh whose range pins
the newer content. Second test: `git mesh add m a#L1-L10 a#L1-L10` in
one invocation succeeds, one op remains. Third test: the same flow
survives across a `restore` + re-add.

## Slice 4 — Sidecar tamper detection (Critical #4)

**Symptom.** Overwriting `.git/mesh/staging/<name>.<N>` with arbitrary
bytes is invisible to `git mesh commit`, `git mesh stale`, and
`git mesh doctor`.

**Root cause.** Nothing records or verifies a content hash of the
sidecar bytes. The `.meta` file stores a normalization stamp but no
cryptographic digest.

**Fix.**

1. Extend `SidecarMeta` in `src/staging.rs` (and `src/types.rs` if
   that's where the struct lives) with a `content_sha256` field,
   populated in `append_prepared_add` when the sidecar is written.
2. At every sidecar read site, verify `sha256(sidecar_bytes) ==
   meta.content_sha256`. Read sites to patch:
   - `src/mesh/commit.rs` (pre-write validation — fail with a new
     `Error::SidecarTampered { mesh, index }`),
   - `src/resolver/engine/pending.rs:164-181` (`pending_add_drift`),
   - `src/resolver/engine/pending.rs:30-65` (ack loop),
   - any other caller turned up by `grep -rn "sidecar_path\|\.meta\.\|
     SidecarMeta"` after the field is added.
3. Surface the new error class in `git mesh doctor`
   (`src/cli/structural.rs`) — a `SidecarTampered` finding in the same
   style as `DanglingRangeRef`.
4. Make the commit path refuse to proceed on any tampered sidecar
   (non-zero exit), and make `git mesh stale` emit a pending drift of
   a new variant (`PendingDrift::SidecarTampered`) so it's
   distinguishable from the legitimate `SidecarMismatch` bytes-changed
   case.

**Test.** Integration: stage an add, overwrite the sidecar with
garbage, assert:
- `git mesh commit` fails with exit 2 and the new error,
- `git mesh stale` exits non-zero and mentions "tampered",
- `git mesh doctor` reports the finding.

## Slice 5 — `--since` filter (Critical #5)

**Symptom.** `git mesh stale --since <commit-ish>` has no observable
effect. The flag is declared at `src/cli/mod.rs:188` and never read
anywhere in the codebase (verified by `grep -rn 'args\.since\|\.since'
src/`).

**Fix.**

1. Thread the value from `StaleArgs.since` through the stale entrypoint
   (likely `src/cli/stale_output.rs` or wherever `run_stale` lives) to
   the resolver.
2. In the resolver, after loading range records, resolve the
   `--since` commit-ish once via gix. For each range, skip it if its
   `anchor` is an ancestor of `since` (i.e. `since` is NOT an ancestor
   of `anchor`, read as "anchored at or after `since`"). Use gix's
   revwalk / `is_ancestor` primitives.
3. Handle edge cases explicitly:
   - `since` itself equal to an anchor → include (documented "at or
     after").
   - `since` unresolvable → return `Error::Git` with a clear message;
     do not silently fall back.
   - Orphaned anchors → always include (the point of `--since` is
     scoping, not hiding orphans).
4. Emit an annotation in verbose human output so the user can see
   "filtered N ranges anchored before <since>".

**Test.** Integration: two meshes, one anchored on `main@{seed}` and
one anchored on `feat@{HEAD}`; commit drift affecting both; assert
`--since $(git merge-base main feat)` surfaces only the `feat`-anchored
mesh. Additional cases: `--since HEAD` yields zero findings; `--since
<bad-ref>` errors cleanly.

## Slice 6 — Minor quality issues

These are small, should be bundled at the end of the main slices.

### 6a. SIGPIPE panic on piped output

**Symptom.** `git mesh <...> | head` panics with `failed printing to
stdout: Broken pipe (os error 32)`.

**Fix.** In `src/main.rs`, before calling `run()`, install the standard
Unix SIGPIPE handler so a broken pipe becomes a clean exit:

```rust
#[cfg(unix)]
unsafe {
    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
}
```

(or use the `signal-hook` crate if already in the tree; a raw
`libc::signal` is acceptable given this is the main binary entrypoint).
No test needed beyond a manual `git mesh <big-output> | head -1`
returning exit 141 without a stacktrace.

### 6b. Content-blind binary detection on line-range add

**Symptom.** `git mesh add m bin.dat#L1-L1` succeeds when `bin.dat`
contains NUL bytes but has no `binary` attribute.

**Fix.** In `src/staging.rs` (`validate_add_target`) or
`src/types.rs::validate_add_target`, after the attribute check, add a
content sniff: if the first ~8 KiB of the filtered bytes contain a NUL,
reject with the existing "line-range pin rejected on binary path"
error. This mirrors git's own heuristic. Gate behind an opt-in if
teams dislike content sniffing: e.g. `mesh.binaryDetection =
attrs-only | attrs+content` (default `attrs+content`).

**Test.** A file with embedded NUL and no attribute — line-range add
rejected; whole-file add accepted (existing behavior).

### 6c. Duplicate mesh refspec entries on repeat `git mesh push`

**Symptom.** Each `git mesh push` appends another copy of
`+refs/ranges/*:refs/ranges/*` and `+refs/meshes/*:refs/meshes/*` to
`.git/config`.

**Root cause.** `src/sync.rs::ensure_refspec_configured` (lines 83-
122) reads existing refspecs via `get_remote_multi` and filters, then
writes the remaining ones via
`gix::config::File::section_mut_or_create_new` + `section.push`. In
practice the write side isn't idempotent across separate invocations —
likely the gix section handling creates a new subsection rather than
merging, or the read misses multi-valued entries written by a prior
run.

**Fix.**

1. Reproduce in a small unit test: run `ensure_refspec_configured`
   twice and assert the resulting `remote.origin.fetch` has exactly
   two mesh refspecs.
2. If the read side is the culprit (most likely given `get_remote_multi`
   walks `strings_by` which should see all values), patch the write
   side to use `section.set` semantics instead of `push`, or swap to
   the `gix::config::File::set_raw_value` API with append-if-missing
   semantics.
3. Add a one-time migration in `git mesh doctor` that collapses
   duplicate mesh refspecs, so existing affected repos self-heal.

### 6d. Reflog coverage for mesh refs

**Symptom.** Custom refs under `refs/meshes/*` don't get reflog
entries by default; the handbook's §Atomicity claims reflog safety.

**Fix.** When creating the mesh (or on `doctor` init), set
`core.logAllRefUpdates = always` if it isn't already set to a value
that covers custom refs. Do it once, lazily, the first time a mesh
commit is about to be written. Log an INFO finding from `doctor` when
this is configured.

### 6e. `--at` flag only honored when it precedes positional ranges

**Symptom.**
`git mesh add old f.txt#L1-L1 --at HEAD~2` resolves the range against
HEAD (so the path-in-tree check errors on the wrong commit); with
`--at` first it works.

**Root cause.** Likely the clap derive for `AddArgs` lists `ranges:
Vec<String>` before `at: Option<String>` with default argument greed,
so a trailing `--at HEAD~2` gets parsed but its effect on the per-range
anchor is wrong somewhere downstream. Needs a quick read of
`src/cli/mod.rs` near `AddArgs`.

**Fix.**

1. In `src/cli/mod.rs`, annotate the `ranges` field with
   `#[arg(trailing_var_arg = false, allow_hyphen_values = false)]` if
   not already, and `at` with `#[arg(long)]` — explicit parse order.
2. In `src/cli/commit.rs::run_add`, pass `args.at.as_deref()` to
   `prepare_add` unconditionally (the current code does this at line
   67) — the suspicious part is *earlier* (line 56-57) where
   `validate_add_target` is called with only `path, extent` and no
   anchor, falling through to worktree reads. Confirm the stage-time
   check uses the same anchor the commit path will resolve.
3. Smoke test: `git mesh add old f.txt#L1-L1 --at HEAD~2` and
   `git mesh add old --at HEAD~2 f.txt#L1-L1` produce the same mesh.

**Test.** Integration: two orderings produce identical range blobs
(same anchor OID).

## Ordering and gating

1. Slice 1 (LFS line-range) — unblocks CONTENT_UNAVAILABLE coverage
   and the LFS worked example.
2. Slice 3 (duplicate-range) — small, unblocks standard re-anchor
   ergonomics and is a prerequisite for verifying Slice 2 (we need to
   be able to re-add cleanly).
3. Slice 2 (whole-file ack) — depends on sidecar normalization helpers
   that Slice 4 will touch.
4. Slice 4 (sidecar integrity) — touches every sidecar call-site; best
   after Slices 1–3 so their new code paths inherit the check.
5. Slice 5 (`--since`) — independent; can run in parallel with Slice
   4 if two engineers.
6. Slice 6 minors — bundled at the end; trivial relative to the
   above.

Each slice must leave `yarn validate` clean (workspace root). Per-slice
the minimum is `cargo test -p git-mesh && cargo clippy -p git-mesh
--all-targets -- -D warnings` inside `packages/git-mesh/`.

## Out of scope

- Doc bugs listed in `docs/manual-validation-of-git-mesh.md` (handbook
  edits).
- Deferred validation coverage (SUBMODULE legacy status, partial-clone
  / sparse-checkout `CONTENT_UNAVAILABLE` reasons, true CAS race).
- Copy-detection mode effect investigation (Task 11 concern) — needs a
  fixture-rich reproduction before implementation work.
