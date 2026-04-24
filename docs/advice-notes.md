# `git mesh advice <sessionId>` — design notes

Rolling scratch for decisions and open questions while we shape the command.

---

## 0. References

Files and sources consulted while drafting these notes.

**Project context**
- `/workspace/CLAUDE.md` — repo-wide rules (golden-rule validation,
  greenfield, commit-message, workspace conventions, git-mesh blurb).
- `/workspace/docs/git-mesh-the-missing-handbook.md` — long-form handbook
  for `git-mesh`.

**Rust CLI source** (`/workspace/packages/git-mesh/src/`)
- `cli/mod.rs` — clap command surface; `parse_range_address`; dispatch.
- `cli/stale_output.rs` — porcelain format
  (`STATUS \t SOURCE \t MESH \t PATH \t START \t END \t ANCHOR`,
  `SOURCE ∈ {H, I, W, S}` with optional `/ack`).
- `cli/{commit,pre_commit,show,structural,sync}.rs` — other handlers.
- `mesh/`, `resolver/`, `file_index.rs`, `staging.rs`, `range.rs`,
  `types.rs`, `validation.rs`, `sync.rs`, `git.rs`, `lib.rs`, `main.rs`.

**Existing Claude Code hooks** (`/workspace/plugins/git-mesh/bin/`)
- `_lib.sh` — shared helpers (`meshes_for_path`,
  `render_mesh_summary`, `render_stale`, `emit_additional_context`,
  `emit_stop_context`, session cache paths).
- `session-start.sh` — snapshots `git mesh stale --format=porcelain`.
- `user-prompt-submit.sh` — path scraping from prompts.
- `post-tool-use.sh` — mesh surfacing after edits.
- `stop.sh` — diff-against-baseline drift surfacing.
- `test.sh` — harness (not read in detail).

**Git access**
- `gix` — the pure-Rust git library already used by `git-mesh`. All
  git-state queries in advice go through `gix`; no second git
  implementation, no SQLite virtual-table extension.

---

## 1. Goal

Replace `/workspace/plugins/git-mesh/bin/**` hook logic with a single Rust
subcommand that emits generic, developer-readable markdown about mesh state
relative to what a coding session has been touching. The output is not
Claude-Code-specific; any developer running the command gets useful markdown.

## 2. Primary goals

1. **Tell the developer when they are interacting with a mesh.** Mirror the
   current hook behavior: when a touched path/range intersects a mesh, name
   the mesh, its description, and its partner ranges. Nothing clever.
2. **Emit ready-to-run `git mesh` commands** when the session has enough
   signal to propose an action (extend a group, record a rename, capture a
   new group, narrow a range). Everything else is timing.

## 3. Non-goals

- Do not generate or propose a mesh description ("why").
- Do not be prescriptive — surface consequences, don't direct.
- Do not re-emit what a `git mesh` **write** command already prints to the
  developer (staging acknowledgements from `add` / `rm` / `why` / `config`,
  results of `commit` / `mv` / `delete` / `revert`). Output from **read**
  commands — staleness (`git mesh stale`), connections (`git mesh ls`),
  range lists, description text — is explicitly in scope and is what advice
  is built on.
- Do not describe the developer's own action in isolation — see the
  consequence-centric rule (§5).

## 4. Mental model: meshes are latent contracts

Drift is not a warning — it is the point. A mesh exists so that when range A
moves, the developer is routed to its partner range B, decides whether B
needs a matching change, and reconciles the mesh. Advice **surfaces partners
to visit**; nothing reads as an alarm.

## 5. Consequence-centric rule

> Advice describes consequences on partners the developer is not already
> looking at. It never states the developer's own action.

Corollaries:

- A rename / edit / commit appears only as the minimum context needed to
  make the partner line understandable; the **news is the partner**.
- Excerpt depth scales with event certainty: a read of a range gets an
  address only; an edit crossing a mesh range gets a partner excerpt; a
  rename of a referenced asset gets an excerpt plus a runnable command.

## 6. CLI shape

`sessionId` is always required as the first positional — it names the advice
stream. Append-vs-render is split across two verbs, following git's
`<noun> add` idiom (`git add`, `git notes add`, `git remote add`,
`git worktree add`).

```
git mesh advice <sessionId> add --read   <path>[#L<s>-L<e>]
git mesh advice <sessionId> add --write  <path>[#L<s>-L<e>]
git mesh advice <sessionId> add --commit <sha>
git mesh advice <sessionId> add --snapshot
git mesh advice <sessionId>                            # render (bare)
git mesh advice <sessionId> --documentation            # render + how-to
```

- **`add`** appends one typed event to the session state file. Silent by
  design — safe to fire from every hook without flooding stdout.
- **Bare form** renders the current advice view. The render is itself an
  event — a *flush*: it computes the delta, prints it, records a `flush`
  event with the new `additions`, and thereby advances the seen-set. It
  never mutates git or mesh state.

Surface collision: `git mesh add <name> <ranges…>` already exists at the
mesh level. The `advice` noun scopes `git mesh advice add` cleanly — same
coexistence pattern as `git add` vs `git notes add`.

## 7. Stream-of-events model

`git mesh advice` is a sequential pipeline:

```
add events → git mesh advice <sessionId> add --<kind> <arg>
             ↳ append event to session state (silent)

flush     → git mesh advice <sessionId>
             ↳ recompute over accumulated state
             ↳ print *delta* markdown vs what the session has been told
             ↳ record the flush in the event log
             ↳ update the seen-set so the next flush is a fresh delta
```

Both `add` and the bare render are events on the same stream; the render
is a flush that advances the seen-set.

### Session state: SQLite + parallel audit log

Primary store is a per-session SQLite database; a parallel append-only
JSONL file records the same events for audit and post-hoc debugging.

- **Primary store:** `/tmp/git-mesh-claude-code/<sessionId>.db` — SQLite
  in WAL mode. All queries (seen-set lookups, intersection computation,
  flush composition) run as SQL.
- **Audit log:** `/tmp/git-mesh-claude-code/<sessionId>.jsonl` — one line
  per event, written alongside each INSERT. Not load-bearing; can be
  tailed with `tail -f`, grepped, or re-ingested into a fresh DB.
- **Content chunks** (`pre_blob`, `post_blob` on write events): inlined
  as TEXT columns (and inlined in the audit line). No blob-SHA indirection.
- **Concurrency:** SQLite WAL handles multiple writers cleanly; the audit
  log still relies on `O_APPEND` atomicity for lines under `PIPE_BUF`
  (4 KiB) but the DB is the source of truth, so an interleaved audit line
  is recoverable.

### Git access via `gix`

Queries against git state (history, blame, index, worktree) go through
`gix`, the same pure-Rust library `git-mesh` already depends on. No
second git implementation, no `libgit2`, no SQLite virtual-table
extension.

Shape:

- The advice DB holds only session-local tables — events, flush records,
  seen-set, and a per-flush snapshot of mesh state.
- Git-derived facts the flush needs (commit SHAs touching a path,
  historical co-change counts, rename detection) are computed in Rust
  via `gix` and either inlined into the flush computation or staged into
  a throwaway temp table scoped to the flush.
- Cross-file joins that mix session events with git history are done in
  Rust, not SQL: iterate events, call into `gix`, build the result set.
  SQL is used where it helps (seen-set dedup, grouping touched paths);
  procedural code is used where `gix` is the natural API.

This keeps the binary single-git, keeps cross-platform packaging
unchanged, and avoids a supply-chain dependency on an unmaintained
extension.

### Schema sketch

```sql
-- Event stream (parent table)
CREATE TABLE events (
  id         INTEGER PRIMARY KEY,
  kind       TEXT    NOT NULL, -- 'read'|'write'|'commit'|'snapshot'|'flush'
  ts         TEXT    NOT NULL, -- RFC3339
  payload    TEXT    NOT NULL  -- verbatim JSON, mirrored to audit log
);
CREATE INDEX idx_events_kind_ts ON events(kind, ts);

-- Flattened per-kind tables for indexed access
CREATE TABLE read_events     (event_id INTEGER PRIMARY KEY, path TEXT, start_line INTEGER, end_line INTEGER);
CREATE TABLE write_events    (event_id INTEGER PRIMARY KEY, path TEXT, start_line INTEGER, end_line INTEGER,
                              pre_blob TEXT, post_blob TEXT);
CREATE TABLE commit_events   (event_id INTEGER PRIMARY KEY, sha TEXT);
CREATE TABLE snapshot_events (event_id INTEGER PRIMARY KEY, tree_sha TEXT, index_sha TEXT);
CREATE TABLE flush_events    (event_id INTEGER PRIMARY KEY, output_sha TEXT);

-- Seen-set, as append-only rows tied to flushes
CREATE TABLE flush_additions (
  flush_event_id INTEGER NOT NULL REFERENCES flush_events(event_id),
  mesh           TEXT    NOT NULL,
  reason_kind    TEXT    NOT NULL,
  range_path     TEXT    NOT NULL,
  start_line     INTEGER,
  end_line       INTEGER,
  PRIMARY KEY (mesh, reason_kind, range_path, start_line, end_line)
);
CREATE TABLE flush_doc_topics (
  flush_event_id INTEGER NOT NULL REFERENCES flush_events(event_id),
  doc_topic      TEXT    NOT NULL,
  PRIMARY KEY (doc_topic)
);

-- Mesh snapshot populated on each flush from `git mesh ls` / `stale`
CREATE TABLE mesh_ranges (
  mesh        TEXT    NOT NULL,
  path        TEXT    NOT NULL,
  start_line  INTEGER,
  end_line    INTEGER,
  status      TEXT,         -- FRESH|CHANGED|MOVED|ORPHANED|...
  source      TEXT,         -- H|I|W|S, with optional /ack
  ack         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_mesh_ranges_path ON mesh_ranges(path);
CREATE INDEX idx_mesh_ranges_mesh ON mesh_ranges(mesh);
```

### Derived-state queries

- **Seen-set membership** (dedup):
  ```sql
  SELECT 1 FROM flush_additions
   WHERE mesh = ? AND reason_kind = ? AND range_path = ?
     AND start_line IS ? AND end_line IS ?
  ```
- **Touched multiset** (for co-touch / coherence):
  ```sql
  SELECT path, COUNT(*) AS n FROM (
    SELECT path FROM read_events UNION ALL SELECT path FROM write_events
  ) GROUP BY path;
  ```
- **Partners-to-visit for a touched path** (intersection #1):
  ```sql
  SELECT mesh, path, start_line, end_line, status
    FROM mesh_ranges
   WHERE mesh IN (SELECT mesh FROM mesh_ranges WHERE path = ?);
  ```
- **Historical co-change** (intersection #5): computed in Rust via
  `gix` — walk recent commits, collect changed-path sets, count pairs
  restricted to session-touched paths. Result cached on a per-flush
  temp table if needed for downstream joins within the same flush.

### De-duplication rule (load-bearing)

> Once the session has been told about a specific mesh for a specific reason
> at a specific range, do not tell it again.

Reason-keyed, not mesh-keyed: the same mesh may re-surface if the reason
changes. Implemented as a seen-set query against `flush_additions` during
the flush's candidate-subtraction step.

## 8. Input source constraints

All inputs must be reachable via one of:

- the local filesystem (git repo, worktree, `.git/mesh/*`, temp state file),
- the per-session `git mesh advice` state file,
- hook-derived data passed in at invocation time through the typed flags.

No network calls, no external services, no editor-specific APIs.

## 9. Incoming data shapes

The CLI flags in §6 accept a narrow, typed stream. The specific
event-producing system is not load-bearing.

- **Session id** — required positional on every invocation; partitions state.
- **Paths** — repo-relative or absolute.
- **Ranges** — `path#Ls-Le`, or whole-file.
- **Content chunks** — pre- and post-text for write events; used to
  compute the precise post-edit line extent and the structural shape of
  the change.
- **Commit identifiers** — SHAs landed during the session, recorded via
  `add --commit`.
- **Snapshot references** — `tree_sha` + `index_sha`, recorded via
  `add --snapshot`, serving as drift baselines.
- **Timestamps** — set by the CLI at invocation time, not by the caller.

Other hook-available signals (sub-actor ids, cwd, free-text prompts,
batch grouping, event sub-kinds like shell / failure / turn-end) are
latent — see Appendix B.

## 10. Primitives available from git-mesh

- `git mesh ls <path>` — meshes touching a path (via `.git/mesh/file-index`).
- `git mesh stale --format=porcelain` — findings per layer:
  `STATUS \t SOURCE \t MESH \t PATH \t START \t END \t ANCHOR` with
  `SOURCE ∈ {H, I, W, S}` and an optional `/ack` suffix.
- `git mesh <name> --oneline` — range list for a mesh.
- `git mesh why <name>` — durable description text (read-only here).
- `.git/mesh/staging/*` — pending mesh ops.

## 11. Structural intersections (advice heuristics)

Advice is grounded in structural joins between incoming data and existing
mesh / git state. Each intersection becomes a **surfacing reason**; the
dedup key is `(mesh, reason-kind, range)`.

1. **Path ∩ file-index** — partners to visit. Baseline reason: any touched
   path that intersects a mesh surfaces the mesh and its sibling ranges.
2. **Range ∩ mesh ranges on same file** — classify overlap / adjacent /
   disjoint. Overlap and adjacency sharpen the partner signal; disjoint-
   but-nearby is a candidate to grow the mesh range on this side.
3. **Pre→post content delta ∩ stale layers** — predict the status flip
   (`FRESH → CHANGED` at the `W` layer) before the next `stale` recompute.
   If `S` already carries `/ack` for the same range, the reconciliation is
   in flight — do not re-surface.
4. **Commit SHA ∩ mesh anchors** — a mid-session commit whose tree touched
   meshed paths will re-anchor those meshes at post-commit time; preview
   the groups about to move, but only if the developer was not already
   tracking them.
5. **Session-local co-touch ∩ historical co-change** — files the session
   has repeatedly touched together *and* that co-change frequently in
   `git log` but have no mesh: structural new-mesh candidate. Purely
   statistical — no semantic inference, no description guess.
6. **Untracked-file cluster** — new files written together across the
   event stream: structural seed for a new mesh candidate.
7. **Rename of a meshed path + partner textual reference** — the rename
   itself is not advice. But when a partner range contains the old path
   or basename as a literal string (`src="/logo.png"`, `import "./foo"`),
   surface the partner with the concrete text to check.
8. **Edit shrinks a meshed range** — post-edit extent collapses the range
   toward zero lines. Surface the *partner* (the related range is now
   over-specified relative to this one), not the edited side.
9. **Staging-area cross-cuts.** Only surfaces what the developer's own
   command output will not show:
   - a staged `add` range overlaps a range in a *different* mesh;
   - a staged `rm` would leave a mesh empty;
   - a staged `add` records content that differs from the anchor of the
     same range in another mesh.
10. **Coherence check** — touched range is the last `FRESH` range in a mesh
    whose other ranges are all non-FRESH: the mesh is losing coherence;
    narrow-or-delete candidate.
11. **Symbol rename in a meshed range** — the session's edit changed an
    exported name; grep partner ranges for the old name and surface any
    hits. Same consequence-centric framing as #7.

## 12. Output design

### 12.1 Assume zero mesh knowledge

The developer reading this output may never have heard of `git mesh`. The
default message is a mesh header, its description, and a partner list with
bracket status markers — the same shape the hook output already shows. That
is self-explanatory: "these files are related, this one has changed." Prose
clauses, excerpts, and commands only appear when the list alone cannot
convey the signal. A developer who only ever encounters the default never
has to read a paragraph about what a mesh is.

### 12.2 Output lines are commented

Every line of advice output is prefixed with `#` so it reads as a comment
in any shell, log, or diff view into which it is injected. The prefix is
not part of the message — a renderer may strip it — but the canonical form
carries it.

### 12.3 Style

- Short, concise International Business English. Common git jargon (`HEAD`,
  `index`, `worktree`, `staged`, `commit`) is fine — this is a git
  command. Avoid vocabulary that is specific to `git mesh` itself beyond
  the product name.
- Lowercase clauses, no acronyms, no idioms, no CamelCase.
- Digits for numbers (`4 times`, not "four times").
- Plain partner lines are bare addresses; state is carried by bracket
  markers, not prose.

### 12.4 Status markers

The default surfacing shape is:

```
# <mesh-name> mesh: <why>
# - <partner-path>[#L<s>-L<e>] [MARKER]
# - <partner-path>[#L<s>-L<e>]
```

Markers are appended only when non-default. No marker means "still matches
the recorded content at its recorded lines."

| Marker         | Meaning                                                    |
| -------------- | ---------------------------------------------------------- |
| `[CHANGED]`    | Bytes in the range differ from what was last recorded.     |
| `[MOVED]`      | Same bytes found elsewhere in the file.                    |
| `[DELETED]`    | File no longer exists.                                     |
| `[CONFLICT]`   | File is in a merge conflict.                               |
| `[SUBMODULE]`  | File is inside a submodule.                                |
| `[ORPHANED]`   | Mesh range cannot be located.                              |
| `[STAGED]`     | Change is staged but not yet committed.                    |

Clauses (prose after an em-dash) are reserved for states a marker cannot
express — e.g. `— still references "/images/logo.png"` when a partner
holds an old path as a literal.

### 12.5 Density ladder

Three levels. The event picks the level; the mesh state can promote it.

- **L0 — bare partner list.** Mesh header + description + partner
  addresses with status markers. No clause, no excerpt, no command.
  Default for any touch that intersects a mesh.
- **L1 — one excerpt.** L0 plus a short excerpt under the single partner
  that most needs a look. Triggered by a write that crosses the mesh on a
  file different from the excerpted partner.
- **L2 — excerpt plus command.** L1 plus a ready-to-run `git mesh`
  invocation. Triggered only when the action is one command away *and* the
  signal is high: rename literal in a partner, staging cross-cut, losing
  coherence, empty-mesh risk, new-group candidate.

Excerpt rules (L1, L2):

- First 10 lines of the range (whole range if shorter).
- Fence with language inferred from extension (`ts`, `tsx`, `html`, `rs`,
  `py`; plain otherwise).
- Binary / image / LFS / submodule / deleted: no excerpt, address only.
- Truncate lines longer than ~200 chars with a trailing `…`.

### 12.6 Doc-topic preamble

Each message type carries a doc topic (e.g. "recording a new group",
"cross-mesh overlap"). A topic surfaces a single short sentence the first
time an L1 or L2 message that uses it appears in the session; never again.
Tracked via `flush_doc_topics`. A developer who only ever sees L0 never
sees a doc topic.

### 12.7 Ready-to-run commands (L2 only)

Emit a command only at L2 — when the action is one `git mesh` invocation
away. Concrete names and ranges wherever known; a literal `<group-name>`
placeholder for groups that do not yet exist. Short lead-in line
(e.g. `# To record the rename:`), then the command indented with two
spaces, also prefixed with `#`.

Templates:

```
# To add to the group:
#   git mesh add <mesh-name> <path>[#L<s>-L<e>]
#
# To record a new group:
#   git mesh add <group-name> <path-1> <path-2> ...
#
# To record a rename:
#   git mesh add <mesh-name> <new-path>[#L<s>-L<e>]
#
# To remove a range:
#   git mesh rm <mesh-name> <path>[#L<s>-L<e>]
```

### 12.8 Message-type inventory

| ID  | Name                          | Default density | Trigger intersection(s)      | Doc topic              |
| --- | ----------------------------- | --------------- | ---------------------------- | ---------------------- |
| T1  | Partner list                  | L0              | #1, #2, #3                   | (none — L0)            |
| T2  | Partner excerpt on write      | L1              | #2 + write event             | "editing across files" |
| T3  | Rename literal in partner     | L2              | #7                           | "renames"              |
| T4  | Range collapse on partner     | L2              | #8                           | "shrinking ranges"     |
| T5  | Losing coherence              | L2              | #10                          | "narrow or retire"     |
| T6  | Symbol rename hits in partner | L2              | #11                          | "exported symbols"     |
| T7  | New-group candidate           | L2              | #5, #6                       | "recording a group"    |
| T8  | Staging cross-cut             | L2              | #9 (overlap)                 | "cross-mesh overlap"   |
| T9  | Empty-mesh risk               | L2              | #9 (staged rm empties mesh)  | "empty groups"         |
| T10 | Pending-commit re-anchor      | L0              | #4                           | (none — L0)            |
| T11 | Terminal status               | L0              | ORPHANED/CONFLICT/SUBMODULE  | "terminal states"      |

T1 subsumes what was previously split across partner-addresses and
predicted-status-flip — the bracket marker on the partner line carries the
state; no second message is emitted. T10 is an extra marker
(`[WILL RE-ANCHOR]`) on the partner line when a staged commit touches
a meshed path.

### 12.9 Stacking order per flush

1. Doc-topic preamble lines (first-reference only).
2. Per-mesh blocks, grouped by mesh; messages within a block ordered
   T1 → T2 → T3 … (markers before prose clauses before excerpts before
   commands).
3. Cross-cutting blocks last: T7 (new-group candidate), T8 (staging
   cross-cut), T9 (empty-mesh risk).

### 12.10 Worked examples

Read or light edit (T1 / L0):

```
# billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - web/checkout.tsx#L88-L120
# - api/charge.ts#L30-L76 [CHANGED]
```

Write crossing a mesh (T2 / L1):

~~~
# billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - web/checkout.tsx#L88-L120
# - api/charge.ts#L30-L76 [CHANGED]
#
# web/checkout.tsx#L88-L120
# ```tsx
# const res = await fetch("/api/charge", {
#   method: "POST",
#   body: JSON.stringify({ amount, currency, token }),
# });
# ```
~~~

Image rename affecting partner markup (T3 / L2):

~~~
# homepage-assets mesh: The homepage hero image and the markup that embeds it.
# - index.html#L42-L42 — still references "/images/logo.png"
# - public/images/brand/logo.png
#
# ```html
# <img src="/images/logo.png" alt="Acme" />
# ```
#
# To record the rename:
#   git mesh add homepage-assets public/images/brand/logo.png
~~~

Losing coherence (T5 / L2):

```
# billing/checkout-request-flow mesh: Checkout request flow …
# - api/charge.ts#L30-L76 [CHANGED]
# - web/checkout.tsx#L88-L120 [CHANGED]
# - server/legacy.ts#L1-L40 [DELETED]
#
# To narrow or retire the group:
#   git mesh rm billing/checkout-request-flow server/legacy.ts
```

New-group candidate (T7 / L2):

```
# Possible new group over:
# - edited-file-1.html
# - edited-file-2.html
# Touched together 4 times this session; also co-changed in 9 of the last 40 commits.
#
# To record a new group:
#   git mesh add <group-name> edited-file-1.html edited-file-2.html
```

Staging cross-cut (T8 / L2):

```
# billing/refunds [STAGED] overlaps billing/checkout-request-flow at api/charge.ts#L40-L76.
# - billing/checkout-request-flow: api/charge.ts#L30-L76
# - billing/refunds [STAGED]: api/charge.ts#L40-L90
```

### 12.11 `--documentation`

Appends one short sentence per reason-kind pointing at the reconciling
command, in the same plain register. No prescription.

### 12.12 Doc topics

Each doc topic is a short, self-contained block prepended the first time
an L1 or L2 message that uses it fires in the session. Tracked via
`flush_doc_topics`; never repeats. Commands are shown with placeholders
(`<name>`, `<path>`, `<s>`, `<e>`) so the topic stays reusable across
meshes. T1 and T10 have no topic — their L0 shape is self-explanatory.

**Baseline** — fires on the first L1 or L2 of any kind.

```
# A mesh names a subsystem, flow, or concern that spans line ranges in
# several files. The mesh description states what those ranges do
# together. Invariants, caveats, and ownership belong in source comments,
# commit messages, CODEOWNERS, and PR descriptions — not here.
#
# Inspect a mesh:
#   git mesh show <name>           # ranges, description, history
#   git mesh ls <path>             # meshes that touch a file
#   git mesh stale                 # ranges whose bytes have drifted
```

**T2 — "editing across files"**

```
# When a range in a mesh changes, the other ranges in the same mesh may
# need matching changes. The excerpt below is the related range — the
# code on the other side of the relationship. Compare, then either update
# it or accept that the relationship has shifted and re-record the mesh.
#
# A second `git mesh add` over the identical (path, extent) is a
# re-record — last-write-wins, no `rm` needed:
#
#   git mesh add <name> <path>#L<s>-L<e>
#   git mesh commit <name>              # finalized by the post-commit hook
```

**T3 — "renames"**

```
# A related range contains the old path as a literal string. A renamed
# file still works for callers that import it by symbol, but hard-coded
# paths — markup src, fetch URLs, doc links — do not follow a rename.
# Update the literal, or move the mesh to the new path:
#
#   git mesh rm  <name> <old-path>
#   git mesh add <name> <new-path>
#   git mesh commit <name>
```

**T4 — "shrinking ranges"**

```
# The edit reduced a range to far fewer lines than were recorded. The
# mesh now pins less code than the relationship was about. When the line
# span changes, remove the old range first, then add the new one:
#
#   git mesh rm  <name> <path>#L<old-s>-L<old-e>
#   git mesh add <name> <path>#L<new-s>-L<new-e>
#   git mesh commit <name>
```

**T5 — "narrow or retire"**

```
# Most ranges in this mesh no longer match what was recorded. When most
# of a mesh has drifted, the relationship itself has usually changed.
# Narrow the mesh to the ranges still in play, or retire it:
#
#   git mesh rm     <name> <path>          # drop a range
#   git mesh delete <name>                 # retire the mesh
#   git mesh revert <name> <commit-ish>    # restore a prior correct state
```

**T6 — "exported symbols"**

```
# An exported name changed inside one range. Other ranges reference the
# old name as a literal string, which a rename-aware refactor tool will
# not reach. Update the references, then re-record both sides in the
# same commit:
#
#   git mesh add <name> <path>#L<s>-L<e>
#   git mesh commit <name>
```

**T7 — "recording a group"**

```
# These files move together: the session has touched them together and
# git history shows them co-changing. A mesh captures that so future
# edits on one side surface the others. Only record one if the
# relationship is real and not already enforced by a type, schema,
# validator, or test — those reject violations automatically and are
# strictly better than a mesh over the same surface.
#
# Record:
#   git mesh add <group-name> <path-1> <path-2> [...]
#   git mesh why <group-name> -m "What the ranges do together."
#   git mesh commit <group-name>
#
# Name with a kebab-case slug that titles the subsystem, optionally
# prefixed by a category: billing/, platform/, experiments/, auth/.
# One relationship per mesh — if ranges split into two reasons to change
# together, record two meshes.
```

**T8 — "cross-mesh overlap"**

```
# A range staged on one mesh overlaps a range already recorded on
# another mesh in the same file. Both meshes will observe edits to the
# shared bytes independently. Confirm both relationships are real; if
# they describe the same thing, collapse them:
#
#   git mesh restore <name>                # drop staged changes on a mesh
#   git mesh delete  <name>                # retire the redundant mesh
```

**T9 — "empty groups"**

```
# The staged removal would leave this mesh with no ranges. A mesh with
# nothing in it cannot surface drift. Either add a replacement range in
# the same commit, or retire the mesh:
#
#   git mesh add    <name> <path>[#L<s>-L<e>]
#   git mesh delete <name>
```

**T11 — "terminal states"**

```
# A terminal marker means the resolver cannot evaluate this range at all.
#
# [ORPHANED]  — the recorded commit is unreachable. Usually a force-push
#               or a partial clone. Fetch and re-record if needed:
#                 git fetch --all && git mesh fetch
#                 git mesh add <name> <path>#L<s>-L<e>
#                 git mesh commit <name>
#
# [CONFLICT]  — the file is mid-merge. Finish the merge first.
#
# [SUBMODULE] — the range points inside a submodule, which mesh does not
#               open. Pin the submodule root or a parent-repo path that
#               witnesses the same relationship:
#                 git mesh rm  <name> <submodule>/inner/file.ts#L10-L20
#                 git mesh add <name> <submodule>
#                 git mesh commit <name>
```

---

## 13. Open questions

- How long to keep `<sessionId>.db` / `.jsonl`; GC policy (time-based sweep
  on `git mesh doctor`? SessionEnd-triggered compaction to a summary?).
- Under `--documentation`, is the preamble longer, or just per-reason hints?
- Threshold for "Session-local co-touch" to count as a new-mesh candidate
  (minimum touches, historical-co-change lift).
- Threshold for "collapse" (what percentage of the anchored extent).
- Whether any event kind should reset the seen-set besides compact /
  session-end (e.g. a branch switch reflected in `--commit <sha>`).
- **Dedup strategy.** The flat seen-set tuple `(mesh, reason_kind, range)`
  is too coarse: a third touch in the same mesh carries more signal than
  the second, and the dedup rule would suppress it. Three candidate
  strategies, undecided:

  1. **Trigger-aware tuple.** Extend the dedup key to
     `(mesh, reason_kind, partner_range, trigger_range)` — re-surface when
     the *trigger* range (the range just touched by the developer) is new,
     even if the partner range was shown before.
  2. **Trigger-aware + coverage-driven density ramp.** Same tuple as (1),
     plus track per-mesh coverage (fraction of mesh ranges the session has
     touched). When coverage grows, re-surface the whole mesh with
     elevated density (`light → regular → verbose`). Ties confidence to
     the message-type density ladder.
  3. **Known-state snapshot.** Replace dedup entirely with a per
     `(mesh, reason_kind)` snapshot of session knowledge
     (`{touched_ranges, drifting_ranges, commit_count, …}`). Re-surface
     when the current snapshot is a proper superset of any prior. Richer,
     more machinery, generalizes to custom signals.

  No decision yet. The schema in §7 reflects the coarse tuple; whichever
  strategy wins will require a column-level revision.

---

## Appendix A. Existing hook behavior (to be replaced)

- `session-start.sh` — snapshots `git mesh stale --format=porcelain` to
  `/tmp/git-mesh-claude-code/<sessionId>.txt`.
- `user-prompt-submit.sh` — scrapes paths from the prompt and injects
  relevant meshes + drift markers as `additionalContext`.
- `post-tool-use.sh` — same shape, keyed off the edited file path.
- `stop.sh` — diffs current stale output against the baseline and injects
  only new findings.
- Shared helpers in `_lib.sh` (`meshes_for_path`, `render_mesh_summary`,
  `render_stale`).

## Appendix B. Candidate inputs (brainstorm)

Kept as a reference menu; not all are load-bearing for the initial cut.

**Wider git state**

- `git diff --cached` (index vs HEAD) — what's about to be committed.
- Untracked-file set — the usual site of new-mesh signals.
- Reflog — branch switches / resets change the baseline.
- Stash entries created this session.
- `git merge-base <main> HEAD` — scope "new on branch" vs "old on branch".
- Fetched remote mesh refs — upstream drift relative to local.
- `git blame` / `git log -L` on touched ranges — recency and authorship.
- `.gitattributes`, submodule and LFS markers.

**Mesh-internal state**

- All four layers of `git mesh stale` (H / I / W / S).
- `.git/mesh/staging/*` — in-flight ops the caller may not remember.
- `.git/mesh/file-index` — fast path from touched file to candidate meshes.
- `git mesh doctor` findings.
- Mesh-description history (`git mesh why <m> --at HEAD~N`).
- Prior `advice` state files for the same repo (cross-session co-touch).

**Hook-derived, beyond file paths**

- Event kind — read / write / shell / failure / turn-end / compact.
- Event ordering and timing — causal chain of touches.
- Actor grouping — sub-actor clusters imply new-mesh candidates.
- Prompt / task-title text, mined for path tokens only.
- `cwd` changes — narrow scope to a package.

**Derived / computed**

- Co-edit pairs across the last N commits.
- `rg`-based import/export scan between touched files and the rest of the
  tree.
- Filename conventions (`foo.ts` ↔ `foo.test.ts`; `schema.*` ↔ generated
  consumers).
- CODEOWNERS overlap.
- AST / tree-sitter parse — snap suggested ranges to function / class
  boundaries; derive a local call graph; detect exported-symbol signature
  changes.
- Symbol-level diff — changed exported symbols between anchor and worktree.
- String-literal / constant overlap — same route, event name, table,
  feature-flag id in two files.
- Structural diff similarity — parallel shape changes.
- Statistical co-change lift over base rates.
- Typed-language metadata (`tsc --listFilesOnly`, `rustc --emit=dep-info`,
  LSP) when available.
- Schema files (Protobuf / OpenAPI / GraphQL / JSON-Schema) and their
  consumers.
- Cross-mesh overlap — ranges in multiple meshes suggest split or merge.
- Churn heatmap — commits per file over the last N days.
- Content similarity (shingles / minhash) between candidate range and
  existing mesh ranges.
- Comment pointers — `// see also <path>:<line>`, `@see`, doc links.
- Lockfile / generated-file suppressor.
- Rename / move detection via `git log --follow` or rename-score diff.
- Existing mesh description fuzzy-match against touched filenames.
- Binary / image / LFS classification.

**Self-referential**

- Prior advice output in this session — diff-against-last-render.
