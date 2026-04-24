# Plan: layered staleness for `git mesh stale`

## Goal

`git mesh stale` is the most common verb in the tool. Today it reports drift only against committed history (HEAD), which means a developer who edits code inside a pinned range sees nothing from `stale` until they commit. The refactor makes `stale` a complete "what remains to freshen" view across four layers of state, with three subtractive flags to peel layers off:

- `--no-worktree`
- `--no-index`
- `--no-staged-mesh`

HEAD is always on; it is the floor. There is no convenience alias for "all three off" — callers (including CI) pass the flags explicitly.

## Layers

1. **HEAD** — committed history (today's behavior).
2. **Index** — contents staged with `git add`.
3. **Worktree** — unsaved/unstaged bytes on disk.
4. **Staged mesh ops** — pending entries under `.git/mesh/staging/`.

---

## Behavior

### B1 — Findings are always printed

Every enabled layer's drift findings appear in `git mesh stale` output. There is no quiet mode for non-HEAD layers.

### B2 — Staged re-anchors acknowledge drift

A **staged re-anchor** is a staged mesh op (`git mesh add <name> <path>[#L<s>-L<e>]`) whose sidecar bytes match the current live content for the referenced range. It represents the developer saying "I've seen this drift and the mesh update is queued."

- Only mesh-layer actions create acknowledgments. `git add` and `git commit` never acknowledge mesh drift; they only move file drift between HEAD / index / worktree layers.
- **Matching key.** A staged op acknowledges a `Finding` when its `range_id` matches. `range_id` is the stable key; `(path, extent)` shifts under `Moved` findings and must not be used for matching.
- **Comparison.** Both sides of the comparison are read through the normalization pipeline (D3) before comparison — whole-file *and* line-range. A sidecar captured on one side of a CRLF/LF change must still acknowledge correctly.
- **Sidecar freshness stamp.** Each sidecar carries a normalization version (`gitattributes` SHA-1 + filter driver list hash). Mismatch at `stale` time means the sidecar was captured under different filter rules; the engine re-normalizes both sides before comparing rather than trusting the stored bytes.
- An acknowledgment is invalidated the moment live content drifts from the sidecar.

### B3 — Exit code

Non-zero iff any of the following is true:

1. A `Finding` has `source.is_some()` (drift at HEAD / Index / Worktree) and no matching staged re-anchor.
2. A `Finding` has a terminal status (`Orphaned`, `MergeConflict`, `Submodule`, `ContentUnavailable`) that isn't otherwise suppressed.
3. A `PendingFinding` has `drift: Some(SidecarMismatch)` — the sidecar's normalized bytes disagree with the blob it claims to anchor.

`PendingFinding::Why` and `PendingFinding::ConfigChange` **never** drive exit code; they are informational and render in their own section. Only `Add`/`Remove` ops carry a sidecar and thus a possible mismatch.

Modifiers:

- `--no-exit-code` (existing flag) forces exit 0 regardless of findings.
- `--ignore-unavailable` downgrades `ContentUnavailable` findings so they print but do not drive exit code; other findings unaffected.
- Passing all three `--no-*` flags suppresses non-HEAD layers, so exit code reduces to HEAD-only — the CI invariant.

### B4 — HEAD-only is the CI fast path

Passing `--no-worktree --no-index --no-staged-mesh` (or building `LayerSet::committed_only()` programmatically) skips the `diff-index` call, the `diff-files` call, and the staging-directory walk. CI performance must not regress vs. today.

---

## Data model

### D1 — `RangeStatus`

```rust
pub enum RangeStatus {
    Fresh,
    Moved,
    Changed,
    Orphaned,                              // anchor commit unreachable
    MergeConflict,                         // no stage-0 index entry
    Submodule,                             // path is a gitlink; rejected at `add`, surfaces if legacy
    ContentUnavailable(UnavailableReason), // content should exist but isn't readable locally
}
```

The last four are terminal: no `source`, no slice math, the engine prints them and moves on.

### D2 — Range extent (line-range vs. whole-file)

```rust
pub enum RangeExtent {
    Whole,
    Lines { start: u32, end: u32 },
}
```

`Range` carries `extent: RangeExtent` in place of today's `start`/`end`. This is a greenfield format change (see §Non-goals); existing records migrate as `RangeExtent::Lines`.

- **CLI.** Omit the `#L...` suffix for whole-file: `git mesh add web hero.png`. Line-range form (`#L1-L100`) unchanged.
- **Resolver.** Whole-file compares blob OIDs at the deepest enabled layer — equal → `Fresh`, different → `Changed`, no slice math, no per-line culprit. Renames still produce `Moved` via the walker.
- **Symlinks.** A symlink's blob is the target-path string. Filters never run on symlinks. The worktree-layer read resolves via `readlink` to the target path string and compares as raw bytes to the blob. Line-range pins on symlinks are rejected at `git mesh add`; whole-file pins are allowed and detect retargeting.
- **Submodules.** Detected via `git ls-files --stage` mode-160000 entries. Three cases at `git mesh add`:
  - *Line-range on a path inside a submodule:* rejected. Multi-repo content resolution is out of scope.
  - *Whole-file on a path inside a submodule:* also rejected — the file blob lives in the submodule's object database, which we don't open.
  - *Whole-file on the submodule root (the gitlink path itself):* allowed. Compares gitlink SHAs across layers:
    - HEAD layer: the tree entry at the gitlink path.
    - Index layer: the mode-160000 entry in `.git/index`.
    - Worktree layer: same as index (there is no worktree-byte view of a gitlink; `ContentRef::WorktreeFile` is not used). A staged `git submodule update` that moves the gitlink surfaces as index-layer drift; an unstaged move does not exist in git's model.

  Any legacy range that slips past the precheck and reaches the resolver surfaces as `RangeStatus::Submodule`.
- **Binary paths.** Line-range pinning rejected at `git mesh add`; whole-file allowed.

### D3 — Content normalization

All reads go through git's attribute + filter pipeline (line endings, smudge/clean, LFS) so worktree and index bytes compare against blob bytes in a single canonical form. Raw filesystem reads are never used for comparison.

Reader routing by filter type, resolved from `.gitattributes` at read time:

- **Core filters** (`core.autocrlf`, `text=auto`, `eol=crlf|lf`, `ident`) go through `gix` worktree machinery — native Rust, no subprocess.
- **`filter=lfs` paths** go through a managed `git-lfs` subprocess (`GIT_LFS_SKIP_SMUDGE=1`, long-lived via `git-lfs filter-process` protocol). `gix` has no LFS support; this is a first-class reader, not a fallback. Detection is by `.gitattributes` attribute, not blob sniffing.
- **Other `filter=<name>` drivers** (clean/smudge shell commands, process filters like git-crypt) go through a managed `git filter-process`-protocol subprocess. Long-lived; one process per driver per `stale` run.
- **Symlinks** do not run filters. git stores the target-path string as the blob; the resolver reads the symlink's recorded blob directly and, for the worktree layer, reads the link target (not the pointed-at file) as a string. "Read through filter pipeline" does not apply here.

The **pre-Phase-1 audit** (Phase 0) confirms `gix` coverage for the core-filter set above and produces the filter-process orchestration design for the LFS and custom-driver readers. Acceptance: every fixture in Phase 1's test matrix reads identically to `git cat-file --filters` / on-disk reads via `git diff`.

### D4 — Content unavailability

When the resolver knows content should exist but cannot read it locally without a network call, it surfaces `ContentUnavailable` with a reason. No auto-fetch is ever performed.

```rust
pub enum UnavailableReason {
    LfsNotFetched,
    LfsNotInstalled,
    PromisorMissing,          // partial clone, blob not fetched
    SparseExcluded,           // sparse-checkout excluded path
    FilterFailed { filter: String },
    IoError { message: String },
}
```

**LFS text files.** First-class. `git mesh add` requires content locally cached; it reads real bytes through the managed `git-lfs` reader (D3) to take the anchored slice and stores the pointer blob OID in the range's existing `blob` field. If content isn't cached, `git mesh add` fails with a `LfsNotFetched`-shaped error pointing at `git lfs fetch`.

At `stale` time:

- *Fast path — pointer OID equal at the deepest enabled layer:* `Fresh`. No LFS reader invoked.
- *Pointer OID changed, both sides cached:* run the managed `git-lfs` reader on both; slice and compare as usual.
- *Pointer OID changed, either side missing:* `ContentUnavailable(LfsNotFetched)`. No auto-fetch.
- *LFS pointer in one layer, smudged content in another* (e.g. pointer in HEAD, smudged bytes not yet committed in worktree): each layer's reader resolves independently; the pointer-OID fast path compares blob OIDs layer-by-layer, not layer-against-HEAD.

**LFS + rename.** If pointer OIDs match across a rename (`Moved` + `Fresh` content), the resolver emits `Moved` — the mesh record's anchored path is out of date; the user is expected to re-anchor via `git mesh add` to refresh the path. The resolver does not silently rewrite the stored path.

**LFS binary files.** Use whole-file pinning; pointer-OID equality is the whole-file check.

### D5 — Duplicates and overlaps in staged ranges

- **Overlapping** line ranges (e.g. `foo#L1-L10` and `foo#L5-L15`) are allowed. No check rejects them.
- **Identical duplicates** (same `(path, extent)` staged twice in one mesh) are last-write-wins: the later op supersedes the earlier. This is a behavior change from today's `git mesh commit`, which errors on duplicate-location — the three `Error::DuplicateRangeLocation` sites in `mesh/commit.rs` become dedup passes and the error variant is removed.
- **"Later" tiebreaker.** Staged ops live at `.git/mesh/staging/<mesh>.<N>` with an integer `N` that increases per add. Dedup orders by `N` descending and keeps the first; ties (impossible under current allocation but possible if a crash leaves a stale file) break by filesystem mtime, then by lexicographic suffix.
- Neither overlaps nor identical duplicates affect `stale` exit code. Rationale: the natural edit-and-re-stage workflow produces duplicate adds on purpose (refreshing the sidecar); rejecting that is hostile.

### D6 — Merge conflicts

Paths without a stage-0 index entry resolve to `MergeConflict` (no slice math). Terminal, printed, drives exit code per B3.

---

## Architecture

```
cli/stale.rs ────┐
pre-commit hook ─┴──▶ resolver::Engine(LayerSet, Scope) ──▶ Vec<Finding> + Vec<PendingFinding>
                                       │
                                       ├── walker      (history traversal, anchor..HEAD)
                                       ├── layers      (HEAD / index / worktree readers, normalized)
                                       ├── attribution (blame, HEAD-source only)
                                       └── pending     (git index hunks ∪ .git/mesh/staging)
```

The engine is the single place drift is computed. `stale` and the pre-commit hook are thin filters over it. `git mesh status` is removed.

---

## Key types

```rust
// types.rs

pub struct LayerSet { pub worktree: bool, pub index: bool, pub staged_mesh: bool }
impl LayerSet {
    pub fn full() -> Self;            // all true
    pub fn committed_only() -> Self;  // all false
}

pub enum Scope { All, Mesh(String), Range(String) }

pub enum DriftSource { Head, Index, Worktree }
// No StagedMesh variant. Staged-mesh-layer disagreement is carried on PendingFinding::drift,
// not as a DriftSource on Finding.

pub enum ContentRef {
    Blob(gix::ObjectId),     // HEAD or index; reader dispatched by .gitattributes filter
    WorktreeFile(PathBuf),   // on-disk; clean filter applied to match blob form
    Sidecar(PathBuf),        // .git/mesh/staging/<mesh>.<N>; re-normalized on read against current filters
}
// ContentRef::read_normalized() -> Result<Vec<u8>>.
// Callers slice into &[&str] on demand — preserves today's zero-allocation hot loop.

pub struct Hunk {
    pub old: (u32, u32),     // (start, count) in the source blob
    pub new: (u32, u32),     // (start, count) in the destination blob
}

pub struct Culprit {
    pub commit: gix::ObjectId,
    pub author: String,
    pub summary: String,
}

pub struct StagedOpRef {
    pub mesh: String,
    pub index: usize,         // index into PendingState.mesh_ops
}

pub enum PendingDrift {
    SidecarMismatch,          // sidecar bytes disagree with claimed blob under current filters
}

pub enum PendingFinding {
    Add          { mesh: String, range_id: String, op: StagedAdd,    drift: Option<PendingDrift> },
    Remove       { mesh: String, range_id: String, op: StagedRemove, drift: Option<PendingDrift> },
    Why          { mesh: String, body: String },          // no drift field; never drives exit code
    ConfigChange { mesh: String, change: StagedConfig },  // no drift field; never drives exit code
}

pub struct RangeLocation {
    pub path: PathBuf,
    pub extent: RangeExtent,
    pub blob: Option<gix::ObjectId>,  // Some when path has a blob at that layer; None for worktree-only reads, submodule gitlinks (see D2), and terminal statuses where no blob resolves
}

pub struct Finding {
    pub mesh: String,
    pub range_id: String,
    pub status: RangeStatus,
    pub source: Option<DriftSource>,          // None when Fresh or when status is terminal
    pub anchored: RangeLocation,              // always populated from the pinned Range record
    pub current: Option<RangeLocation>,       // None when Orphaned / Submodule / ContentUnavailable; populated with best-effort path for MergeConflict
    pub acknowledged_by: Option<StagedOpRef>, // staged re-anchor matched by range_id
    pub culprit: Option<Culprit>,             // only when source == Some(Head)
}

pub struct StagedIndexEntry {
    pub blob: gix::ObjectId,
    pub hunks: Vec<Hunk>,    // from `git diff-index --cached -U0 -M HEAD`
}

pub struct PendingState {
    // stage-0 entries only; conflicted paths omitted (engine surfaces MergeConflict instead)
    pub index: HashMap<PathBuf, StagedIndexEntry>,
    pub mesh_ops: Vec<StagedOp>,
}
```

### Field population by `(source, status)`

| Status            | `source`    | `current`           | `current.blob`                          | `acknowledged_by` | `culprit` |
|-------------------|-------------|---------------------|-----------------------------------------|-------------------|-----------|
| `Fresh`           | `None`      | `Some(loc)`         | `Some(oid)` at deepest enabled layer    | n/a               | `None`    |
| `Moved`           | `Some(H/I/W)` | `Some(loc)`       | `Some(oid)` where resolvable            | optional          | HEAD only |
| `Changed`         | `Some(H/I/W)` | `Some(loc)`       | `Some(oid)` for Head/Index; `None` for Worktree-only | optional | HEAD only |
| `Orphaned`        | `None`      | `None`              | —                                       | `None`            | `None`    |
| `MergeConflict`   | `None`      | `Some(loc)` path only | `None`                                | `None`            | `None`    |
| `Submodule`       | `None`      | `None`              | —                                       | `None`            | `None`    |
| `ContentUnavailable` | `None`   | `None`              | —                                       | `None`            | `None`    |

Worktree content has no blob OID — `gix` doesn't synthesize one and neither do we. Comparisons on the worktree side use normalized bytes directly.

---

## Non-goals

- No generalization to N arbitrary layers. Four fixed layers.
- No backwards-compatibility shims. Greenfield; change call sites and on-disk formats in place as needed (per the `<greenfield>` directive in `CLAUDE.md`).
- No auto-fetch of any kind — no LFS fetch, no submodule fetch, no promisor fetch. `stale` is a local query.
- No multi-repo resolution. Files inside submodules are rejected at `git mesh add`.

---

## Implementation phases

Each phase ends green under `yarn validate` per the `<golden-rule>` in `CLAUDE.md`. Integration tests live in `packages/git-mesh/tests/stale_mesh_integration.rs`; renderer snapshot tests live alongside `cli/stale_output.rs`.

### Phase 0 — Filter-pipeline audit and reader design (prerequisite)

Before any code changes, two outputs are required.

**gix core-filter audit** — verify `gix` worktree-filter coverage for: `core.autocrlf=true|input`, `text=auto`, `eol=crlf|lf` per `.gitattributes`, `ident` expansion. Output: a short findings doc under `docs/`. Any gap in a core filter triggers a targeted subprocess fallback for that filter only.

**Non-gix reader design.** `gix` has no LFS support and incomplete coverage for arbitrary `filter=<name>` process-protocol drivers. Design (not implement — Phase 1 does) the orchestration for:

- A managed `git-lfs filter-process` subprocess for `filter=lfs` paths, spawned lazily on first LFS read, reused across a `stale` run, torn down on exit. `GIT_LFS_SKIP_SMUDGE=1` in its environment.
- A managed `git filter-process`-protocol subprocess per custom driver (`filter.<name>.process`). Also lazy, also reused.
- Detection: `.gitattributes` attribute lookup per path via `gix`'s attribute stack. No blob sniffing, no PATH probing for `git-lfs` — if the attribute says `filter=lfs` and the subprocess can't start, surface `ContentUnavailable(LfsNotInstalled)`.

Acceptance: every fixture in Phase 1's test matrix reads identically to `git diff`'s view of the same paths. Measured byte-for-byte, no wiggle.

### Phase 1 — Layered engine + renderers + add-time prechecks

One shipping unit. The engine, the renderers, and the `git mesh add`-time validation move together because Phase 1's new types (`Finding`, `PendingFinding`, `RangeExtent`) can't exist without a renderer that emits them and without a stager that produces them. No module split yet — that's Phase 2, following the seams this phase exposes.

**Types.** Add to `types.rs`: `LayerSet`, `Scope`, `DriftSource`, `ContentRef`, `Hunk`, `Culprit`, `StagedOpRef`, `PendingDrift`, `Finding`, `PendingFinding`, `PendingState`, `StagedIndexEntry`, `RangeExtent`, `UnavailableReason`. Extend `RangeStatus` with `MergeConflict`, `Submodule`, `ContentUnavailable(UnavailableReason)`. Replace `Range`'s `start`/`end` with `extent: RangeExtent`. Change `RangeLocation.blob` to `Option<gix::ObjectId>`.

**Readers.** Build the reader dispatch per D3 (gix core + LFS subprocess + custom filter-process subprocess + symlink direct-read). Route every existing HEAD-layer read (`stale.rs:72, 77, 148, 149`) through `ContentRef::Blob(...).read_normalized()`.

**Engine.** Reshape `resolve_range_inner`:

```
tracked := resolve_at_head(repo, range)
if layers.index     { tracked := apply(index_hunks, tracked) }
if layers.worktree  { tracked := apply(worktree_hunks, tracked) }
content  := read_normalized(content_ref_for(tracked, deepest_enabled_layer))
compare anchored slice vs content slice
source   := first layer (shallowest) where content diverges; None if Fresh
```

- Index / worktree diffs: single **unscoped** `git diff-index --cached -U0 -M HEAD` and `git diff-files -U0 -M`. Parse full output; keep entries whose source path or rename destination is in `file_index`.
- **Rename-budget cap.** If the changeset exceeds 1000 paths, rerun the diffs with `--no-renames` and flag affected findings with a note rather than paying O(A × D). Threshold is a `const`; override via env var for testing.
- Merge-conflict paths (no stage-0 entry) resolve as `MergeConflict`.
- **Acknowledgment matching by `range_id`.** For each `Finding`, find staged ops in `PendingState.mesh_ops` whose `range_id` matches; populate `acknowledged_by` when the sidecar (re-normalized against current filters) matches current live content. Whole-file = blob bytes; line-range = sliced lines.
- Populate `Vec<PendingFinding>` from `PendingState.mesh_ops` when `layers.staged_mesh` is on. For `Add`/`Remove`, compute `drift: Option<PendingDrift>` by comparing sidecar against the claimed blob under current filters.
- Rework `culprit_commit` to blame against the commit that produced `current.blob` (latest commit in `anchor..HEAD` that touched the tracked location). Only runs when `source == Some(Head)` and `current.blob.is_some()`.
- **Concurrency guard.** Read the index file's SHA-1 trailer at start and end of a run (not mtime — sub-second races on ext4/APFS are real). If it changed, print a stderr warning suggesting re-run. Exit code unaffected.

**CLI and `git mesh add` prechecks.** Flags on `cli/stale.rs`: `--no-worktree`, `--no-index`, `--no-staged-mesh`, `--ignore-unavailable`. Independent `bool`s; no `ArgGroup`. On `cli/add.rs` (or wherever `git mesh add` lives), add **stage-time** (not commit-time) validation:

- Reject line-range pins on binary paths (per `.gitattributes`), symlinks, and paths inside a submodule.
- Allow whole-file pins on submodule gitlink paths; reject whole-file pins on paths inside a submodule.
- For `filter=lfs` paths, require local cache; on miss fail with an error carrying the `UnavailableReason::LfsNotFetched` shape (reuse the enum; error messages share vocabulary with `stale` output).
- Errors surface immediately at `git mesh add`, not queued and discovered at `git mesh commit`.

**Dedup.** Replace the three `Error::DuplicateRangeLocation` sites in `mesh/commit.rs` with a last-write-wins pass keyed by `(path, extent)`, ordered by staging `N` descending (D5 tiebreaker). Remove the error variant.

**Renderers.** Rewrite `cli/stale_output.rs` around `Finding` and `PendingFinding`:

- **Human** (default): existing columns plus a `src` column (`H`/`I`/`W`) and an `ack` marker when `acknowledged_by` is populated. `PendingFinding::Why` and `::ConfigChange` render in their own trailing section; `Add`/`Remove` render with a `drift` note when set.
- **Porcelain / JSON / JUnit / GitHub Actions** match today's `--format` list. JSON carries full `Finding` / `PendingFinding` structure under a top-level `{ "schema_version": 1, ... }` envelope; schema documented in `docs/`.

HEAD-only mode's `src` column only ever contains `H` — scripts that don't parse it are unaffected.

**Fast path.** `LayerSet::committed_only()` short-circuits both diff calls, the staging-dir walk, and the reader-dispatch setup for non-core filters.

**Performance gate.** Add a `criterion` bench: HEAD-only `stale` on the existing fixture, measured before and after Phase 1. Regression >10% fails acceptance.

**Acceptance tests (`tests/stale_mesh_integration.rs`):**

- HEAD-only mode: byte-identical output on the existing fixture.
- Worktree-only drift → `Changed`, `source=Worktree`, `current.blob = None`, exit 1.
- `git add` moves drift from Worktree to Index; `current.blob = Some(staged_oid)`; exit still 1.
- `git mesh add` matching sidecar → `acknowledged_by` populated, exit 0.
- Subsequent worktree edit invalidates the ack → exit 1.
- Ack matching survives `Moved`: range's extent shifts, sidecar at old extent still acknowledges via `range_id`.
- Sidecar captured before a `.gitattributes` EOL change: re-normalized on read still acknowledges.
- `git add -p` partial staging: range straddles partial edit; both layers show drift with shifted locations.
- Merge-conflict path → `MergeConflict`, `current.blob = None`.
- CRLF checkout of an LF blob → no false drift.
- Whole-file pin on a binary asset: blob OID change → `Changed`; `git mesh add <name> <path>` re-anchors and acknowledges.
- Whole-file pin on a submodule gitlink: index-layer SHA change (`git submodule update` staged) → `Changed`.
- Whole-file pin on a symlink: retarget → `Changed`. Line-range pin on a symlink is rejected at `git mesh add`.
- LFS text file, content cached: slice-level `Changed`/`Moved` equivalent to non-LFS.
- LFS text file, content missing: `ContentUnavailable(LfsNotFetched)`, exit 1; exit 0 with `--ignore-unavailable`.
- LFS repo with no `git-lfs` binary on PATH: `ContentUnavailable(LfsNotInstalled)`.
- Custom `filter=<name>` driver with broken smudge: `ContentUnavailable(FilterFailed { filter })`.
- `git mv` across a pinned file (one-layer rename): `Moved` with new path; mesh record's anchored path unchanged (re-anchor is a separate action).
- `intent-to-add` path (`git add -N`) with a pinned range: zero-OID index entry; resolver treats as unstaged; new-file variant (no HEAD) falls back to worktree read.
- Rename-heavy changeset (>1000 paths): `stale` completes without pairing blow-up; a note indicates rename detection was disabled.
- Index-file SHA-1 trailer changes mid-run: stderr warning printed; exit code unaffected.

### Phase 2 — Module split

Split the now-larger `stale.rs` along the seams Phase 1 exposed:

- `resolver/walker.rs` — `resolve_at_head`, `advance`, `compute_new_range`, `name_status`, `parse_hunk`, `NS`, `copy_detection_args`.
- `resolver/layers.rs` — index/worktree diff parsing, hunk application, `ContentRef::read_normalized` dispatch, merge-conflict detection, the gix reader, the `git-lfs filter-process` and custom `git filter-process` subprocess orchestrators.
- `resolver/engine.rs` — top-level `resolve_range` / `resolve_mesh` / `stale_meshes`, acknowledgment matching, concurrency SHA-trailer guard.
- `resolver/attribution.rs` — `culprit_commit`, `blame_culprit`, `differing_lines`, `parse_blame`.
- `resolver/mod.rs` — `pub use` re-exports.

Rename `stale.rs` → `resolver/mod.rs`. Callers (`mesh/commit.rs`, `cli/*`) migrate to `crate::resolver::*` in the same PR; no transitional alias.

Acceptance beyond `yarn validate`: each submodule ≤ 400 lines; no cyclic submodule deps; `cargo doc` produces a coherent module tree.

### Phase 3 — Remove `git mesh status`

The layered `git mesh stale [name]` surfaces everything `status` showed plus drift.

- Delete the `status` subcommand, its CLI module, and its tests.
- Delete the duplicate `StagedConfig` / `staging.adds` / `staging.removes` rendering in `mesh/commit.rs`; that data now flows through `PendingFinding::ConfigChange` / `Add` / `Remove`.
- Port substantive tests (duplicate-location detection becomes last-write-wins per D5; config-change rendering) into stale integration tests.
- **Schema superset check.** Any field previously in `status --format=json` is present in `stale`'s JSON under the `schema_version: 1` envelope, or explicitly documented as removed. Release notes call out the `Error::DuplicateRangeLocation` variant removal.
- Update `README.md` and the handbook to point users at `git mesh stale [name]`.

### Phase 4 — Pre-commit hook onto the engine

Pre-commit's real check: any tracked mesh range whose anchored slice diverges from what's about to be committed.

- Run `engine.run(LayerSet { worktree: false, index: true, staged_mesh: true }, Scope::All)`.
- Filter findings to those whose `current` path is in `PendingState.index.keys()`, or whose staged mesh op intersects a staged path.
- Fail the commit iff any filtered finding has `source == Some(Index)` with `acknowledged_by.is_none()`, **or** any pending `Add`/`Remove` has `drift: Some(SidecarMismatch)`.
- Worktree drift is **not** a pre-commit failure.

Delete the duplicate slice-comparison logic in `staging.rs` / `mesh/commit.rs`.

### Phase 5 — Docs

- Rewrite §5 of `docs/git-mesh-the-missing-handbook.md` and the troubleshooting entries at L534 and L554 for the layered default, staged re-anchor acknowledgments, whole-file pinning, and the four new flags (`--no-worktree`, `--no-index`, `--no-staged-mesh`, `--ignore-unavailable`).
- Add a worked `git add -p` example.
- Add a CI recipe snippet using the three `--no-*` flags for HEAD-only mode.
- Add a worked image/copy example demonstrating whole-file pinning.
- Add an LFS text example covering cached / not-cached behavior.

---

## Risks

Concerns *not* already covered by a rule or a phase.

- **`file_index` under layered runs.** Stays HEAD-layer infrastructure. Layered runs pay full path-enumeration cost once per invocation. Re-evaluate only if measured performance is bad.
- **`assume-unchanged` / `skip-worktree`.** Treat as unstaged at the index layer; the range resolves against HEAD (if present) or worktree. Revisit only if a test fixture exposes a user-visible problem.
- **Culprit column ambiguity.** `source` enum in JSON is authoritative; the human column renders `(index)` / `(worktree)` / `(staged)` for non-HEAD. Scripts that parse the culprit cell as a SHA must switch to the `source` field; schema doc calls this out.
- **Case-insensitive filesystems / Unicode normalization.** macOS ships NFD filenames that can mismatch NFC-normalized paths in the git index. Not addressed in Phase 1; a follow-up if it surfaces.
- **Linked worktrees (`git worktree add`).** Each linked worktree has its own index; `.git/mesh/staging/` lives in the main `.git/`. Current design assumes a single worktree; running `stale` inside a linked worktree reads the linked worktree's index but the main worktree's staged mesh ops. Acceptable for v1; documented limitation.
- **Sparse-checkout path-absence disambiguation.** A path missing from the worktree could mean `SparseExcluded`, `IoError`, or a legitimate uncommitted deletion. Detection order: sparse-checkout cone state (via `gix`) → stat syscall → diff entry. `SparseExcluded` wins when the path is excluded by sparse config regardless of what's on disk.
- **`.gitignore` interaction with whole-file pins.** A pin on an ignored path is allowed (pinning an ignored file is a valid workflow); `stale` reads it like any other. Documented, not enforced.

---

## Out-of-scope follow-ups

- `MeshConfig.default_layers` to let a repo pin a different layer default.
- `--only-worktree-drift`-style filter flags (orthogonal to layer selectors).
- IDE integration (VS Code extension) surfacing worktree-layer findings inline; depends on Phase 1's JSON renderer.
- Multi-repo resolution for submodule contents.

---

## Validation

Every phase ends with `yarn validate` from the workspace root, green. No phase is done until workspace-level validation is clean — per the `<golden-rule>` in `CLAUDE.md`.
