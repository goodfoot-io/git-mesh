# `git mesh advice` — manual evaluation findings

Behavior gaps, bugs, and documentation mismatches observed against
`docs/advice-notes.md`. Each entry includes minimal repro steps. Setup
common to most repros:

```bash
mkdir /tmp/eval-advice && cd /tmp/eval-advice && git init -q
printf "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n" > a.ts
printf "// b1\n// b2\n// b3\n// b4\n// b5\n// b6\n// b7\n// b8\n// b9\n// b10\n" > b.ts
git add . && git -c user.email=a@a -c user.name=a commit -qm init
git-mesh add demo-pair a.ts#L1-L5 b.ts#L1-L5
git-mesh why demo-pair -m "Pair of files for testing."
git-mesh commit demo-pair
```

State files live in `/tmp/git-mesh-claude-code/<sessionId>.{db,jsonl}`;
delete them between scenarios to start fresh.

---

## 1. CLI surface and argument validation

### 1.1 Mesh names cannot use the documented `<category>/<slug>` form
The handbook and `docs/advice-notes.md` §12.12 (T7) both prescribe a
`category/slug` naming convention (`billing/checkout-request-flow`,
`platform/...`). The mesh CLI rejects any name containing `/`.

Repro:
```
$ git-mesh add demo/pair a.ts#L1-L5 b.ts#L1-L5
error: invalid name: `demo/pair` must not contain `/`
```

User impact: every example in the documentation is unrunnable as-is, and
the T7 doc-topic preamble (§12.12) tells the developer to use names that
the tool then refuses.

### 1.2 `git mesh advice --help` requires a git repo
Running outside a git repo cannot get help text:
```
$ cd /tmp && git-mesh advice --help
error: not inside a git repository: ...
```
Help should be free of repo state.

### 1.3 Empty `<sessionId>` accepted; produces a dotfile state file
```
$ git-mesh advice "" add --read a.ts
$ ls -la /tmp/git-mesh-claude-code/.db   # exists, hidden
```
Either reject or namespace; current behavior creates `~/.db`-style
hidden files that escape tooling.

### 1.4 `<sessionId>` with `/` silently rewritten with `_`
```
$ git-mesh advice 'foo/bar' add --read a.ts     # exit 0
$ ls /tmp/git-mesh-claude-code/foo*
foo_bar.db  foo_bar.jsonl
```
`foo/bar` and `foo_bar` collide into the same session store. Not
documented; should either reject or preserve the id verbatim (e.g. percent
encoding) and document the rule.

### 1.5 `--read` / `--write` accept invalid specs without error
All of the following exit 0 and write malformed rows:

```
$ git-mesh advice scenG add --read 'no/such/file.ts'    # path doesn't exist
$ git-mesh advice scenG add --read 'a.ts#L99-L1'        # inverted range
$ git-mesh advice scenG add --read 'a.ts#L1-L9999'      # past EOF
$ git-mesh advice scenG add --read ''                   # empty path
```

Inspecting the DB shows the rows landed verbatim:
```
$ sqlite3 /tmp/git-mesh-claude-code/scenG.db "SELECT * FROM read_events;"
1|no/such/file.ts||
2|a.ts|99|1
3|a.ts|1|9999
4|||
```

`docs/advice-notes.md` does not declare a contract here, but per the
project-wide `<fail-closed>` rule these should be rejected. Leaving them
silent means the audit log records actions that never made physical sense
and downstream intersection logic operates on garbage ranges.

### 1.6 No way to pass pre/post content for `--write`
§9 lists "Content chunks — pre- and post-text for write events; used to
compute the precise post-edit line extent and the structural shape of the
change" as required input. The CLI exposes only `--write <PATH[#Ls-Le]>`
with no flag for `--pre`, `--post`, or stdin. Several intersection
heuristics that depend on pre→post content (#3 status-flip prediction,
#8 range collapse, #11 symbol rename) therefore cannot operate on real
content; current behavior either misfires (see 4.1, 4.2) or is silently
disabled.

---

## 2. `--documentation`

### 2.1 At L0 it adds nothing
```
$ git-mesh advice scenC2 add --read 'a.ts#L1-L5'
$ git-mesh advice scenC2 --documentation
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
```
Identical to the bare flush. §12.11 promises "one short sentence per
reason-kind pointing at the reconciling command" — no sentence appears.

### 2.2 At L1 it duplicates the doc-topic preamble at the bottom
```
$ git-mesh advice scenD add --write 'a.ts#L1-L5'
$ git-mesh advice scenD --documentation
# When a range in a mesh changes, the other ranges may need matching changes.
#
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
#
# b.ts#L1-L5
# ```ts
# // b1
# ...
# ```
#
# When a range in a mesh changes, the other ranges may need matching changes.
```
The preamble line is repeated verbatim at the end. There is no
per-reason hint, no command, and the duplication looks like a render bug.

---

## 3. JSONL audit log is not the "parallel" record §7 promises

§7 ("Audit log") states the JSONL file "records the same events" as
SQLite, and that "Content chunks (`pre_blob`, `post_blob` on write events)
[are] inlined as TEXT columns (and inlined in the audit line)." Observed
JSONL entries are a strict, lossy subset of the SQL payload.

### 3.1 Read events
```
JSONL: {"kind":"read","spec":"foo.ts"}
SQL:   read_events row(path=foo.ts, start=NULL, end=NULL)
        events.payload = {"end_line":null,"path":"foo.ts","start_line":null}
```
The JSONL keeps the raw `spec` string instead of the parsed path/range,
and omits the timestamp the schema sketch documents (`ts TEXT NOT NULL`,
RFC3339).

### 3.2 Snapshot events
```
JSONL: {"kind":"snapshot"}
SQL:   snapshot_events row(tree_sha=482a..., index_sha=15bd...)
```
The JSONL drops both SHAs entirely. A debugger replaying the JSONL cannot
reconstruct the snapshot.

### 3.3 Flush events
```
JSONL: {"documentation":false,"kind":"flush","output_len":0}
SQL:   flush_events row(output_sha=cbf29ce484222325)
```
Different fields on each side. The doc claim that the JSONL can be
"re-ingested into a fresh DB" does not hold against the current shape.

User impact: the audit log is not load-bearing for replay or debugging
in its current form, despite being described as such.

---

## 4. Render output bugs

### 4.1 T4 ("shrinking range") fires on writes that did not shrink anything
```
$ git-mesh add bindemo a.ts#L1-L1            # mesh range is L1-L1
$ git-mesh commit bindemo
$ git-mesh advice scenR2 add --write 'a.ts#L1-L1'
$ git-mesh advice scenR2
... # The edit reduced a range to far fewer lines than were recorded; ...
... # To re-record with the new extent:
... #   git mesh rm demo-pair a.ts#L1-L5
... #   #   git mesh add demo-pair a.ts#L1-L5
```
The write is to the exact recorded extent; no shrink occurred. T4 fires
anyway because there is no pre/post content (see 1.6) so the heuristic
cannot tell.

### 4.2 T4 "re-record" command is malformed and points at the old range
From the same flush as 4.1:
```
# To re-record with the new extent:
#   git mesh rm demo-pair a.ts#L1-L5
#   #   git mesh add demo-pair a.ts#L1-L5
```
Two issues:
- Second line begins with `#   #   git mesh add ...` — a stray `#` makes
  the command look commented out / nested when copy-pasted.
- The `add` line repeats the *old* range (`a.ts#L1-L5`). Per T4
  doc-topic, the `add` should carry the new extent (`<new-s>-<new-e>`).
  No new extent is computed (see 1.6), so the rendered command is a
  no-op round trip rather than the intended re-record.

### 4.3 Partner excerpt printed twice when multiple meshes intersect the same path
```
$ git-mesh add bindemo a.ts#L1-L1   # add second mesh on a.ts
$ git-mesh commit bindemo
$ git-mesh advice scenR2 add --write 'a.ts#L1-L1'
$ git-mesh advice scenR2
... bindemo block ...
... demo-pair block ...
# b.ts#L1-L5
# ```ts
# xxxxxxxx...
# ```
#
# b.ts#L1-L5
# ```ts
# xxxxxxxx...
# ```
```
The same partner excerpt appears under both blocks even though only one
of the two meshes contains b.ts.

### 4.4 Whole-file partner renders an empty fenced block
```
# bindemo mesh: Binary blob.
# - bin.dat
#
# bin.dat
#
```
Per §12.5 binary/whole-file partners should be "address only, no
excerpt." Output instead emits the address a second time as an excerpt
header followed by an empty paragraph, which reads like a missing fence.

### 4.5 `[DELETED]` marker not produced for deleted partner
```
$ rm b.ts && git add -A
$ git-mesh advice scenT add --read 'a.ts#L1-L5'
$ git-mesh advice scenT
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5 [CHANGED]
```
b.ts no longer exists in worktree or index. §12.4 says `[DELETED]`. We
get `[CHANGED]`.

### 4.6 T7 omits the session-touch count promised in the example
§12.10 example renders `Touched together 4 times this session; also
co-changed in 9 of the last 40 commits.` Actual output:
```
# touched together this session; also co-changed in 5 of the last 40 commits.
```
The session-touch count is dropped, and the leading word is lowercase
mid-sentence ("touched") which reads as a fragment.

---

## 5. Intersections that do not surface

### 5.1 Staged meshes are invisible to advice (intersection #9)
Repro after the common setup:
```
$ git-mesh add demo-overlap a.ts#L3-L8 b.ts#L3-L8     # staged, not committed
$ git-mesh why demo-overlap -m "Overlapping mesh."
$ git-mesh advice scenN add --read 'a.ts#L1-L5'
$ git-mesh advice scenN
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
```
Even when the read targets the exact staged range:
```
$ git-mesh advice scenO add --read 'a.ts#L3-L8'
$ git-mesh advice scenO
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
```
No `[STAGED]` marker, no T8 cross-cut block, no acknowledgment that the
staged mesh exists. Per §10 staging is an explicit primitive and §11 #9
calls staging-cross-cuts a first-class reason.

### 5.2 T9 (empty-mesh risk) does not fire for staged removals
```
$ git-mesh rm demo-overlap a.ts#L3-L8
$ git-mesh rm demo-overlap b.ts#L3-L8     # staged removal would empty the mesh
$ git-mesh advice scenP add --read 'a.ts#L1-L5'
$ git-mesh advice scenP
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
```
No T9 block, no command suggesting `git mesh delete` or a replacement
range — consistent with 5.1 (staging area is not consulted).

### 5.3 T3 (rename literal in partner) does not fire
Repro:
```
mkdir /tmp/scenK && cd /tmp/scenK && git init -q
printf '<img src="/images/logo.png" alt="x"/>\n' > index.html
mkdir -p public/images && printf 'PNG\n' > public/images/logo.png
git add . && git -c user.email=a@a -c user.name=a commit -qm init
git-mesh add homepage-assets index.html#L1-L1 public/images/logo.png
git-mesh why homepage-assets -m "Hero image and markup that embeds it."
git-mesh commit homepage-assets

git mv public/images/logo.png public/images/brand-logo.png
git -c user.email=a@a -c user.name=a commit -qm rename
git-mesh advice scenK add --commit $(git rev-parse HEAD)
git-mesh advice scenK
```
Output: empty.

Even reading the partner:
```
$ git-mesh advice scenK3 add --read 'index.html#L1-L1'
$ git-mesh advice scenK3
# homepage-assets mesh: Hero image and markup that embeds it.
# - public/images/logo.png [MOVED]
```
Only a `[MOVED]` marker — no `— still references "/images/logo.png"`
clause, no T3 doc topic, no L2 rename command. The hard-coded path in
index.html is exactly the case the example in §12.10 was written for.

### 5.4 Editing the renamed-to path produces no advice
Continuing scenK above:
```
$ git-mesh advice scenK2 add --write 'public/images/brand-logo.png'
$ git-mesh advice scenK2
   (empty)
```
The renamed-to path is not connected to the mesh (the mesh still anchors
the old path), so a developer who edits the new file gets zero signal
about an existing related mesh and the `index.html` literal still
referencing the old name.

---

## 6. Consequence-centric rule vs documented examples

§5 states "Advice describes consequences on partners the developer is
not already looking at. It never states the developer's own action."
However the §12.10 example for the partner-list message includes the
trigger range itself in the partner list with `[CHANGED]`:
```
# - web/checkout.tsx#L88-L120
# - api/charge.ts#L30-L76 [CHANGED]
```
Observed behavior matches the example, not the rule:
```
$ git-mesh advice scenJ add --write 'a.ts#L1-L5'
$ git-mesh advice scenJ add --read 'b.ts#L1-L5'
$ git-mesh advice scenJ
# demo-pair mesh: Pair of files for testing.
# - b.ts#L1-L5
# - a.ts#L1-L5 [CHANGED]
```
Either §5 or §12.10 should be amended; today the spec contradicts itself
and the implementation matches the example.

---

## 7. Doc-topic preamble divergence from §12.12

Doc topics in §12.12 are multi-paragraph blocks that include reconciling
commands. The implemented preambles are one-line summaries with no
commands:

| Topic | §12.12 first sentence | Actual preamble |
| --- | --- | --- |
| T2 "editing across files" | "When a range in a mesh changes, the other ranges in the same mesh may need matching changes…" + 4 more lines + 2-line code block | `# When a range in a mesh changes, the other ranges may need matching changes.` |
| T4 "shrinking ranges" | 3 sentences + 3-line code block | `# The edit reduced a range to far fewer lines than were recorded; remove the old range and re-add the new extent.` |
| T7 "recording a group" | 6 sentences + 3-line code block + naming guidance | `# These files move together across session touches and recent history; a mesh can capture that.` |

Neither the doc nor the implementation is wrong on its own; they
disagree on what `--documentation` (and the per-topic preamble in
general) is supposed to render. Combined with §2 above, the developer
who runs `--documentation` to "see the how-to" gets a single repeated
sentence and never sees any of the §12.12 commands.

---

## 8. Summary of fix-or-document choices

The user-facing experience deviates from `docs/advice-notes.md` in three
broad ways:

1. **Inputs the spec relies on are not exposed**: pre/post content
   chunks (§9), staging-area awareness (§10, §11 #9), category/slug mesh
   names (§12.12 T7). Several heuristics either misfire or never fire as
   a result.
2. **Output is rougher than the worked examples**: duplicated excerpts
   (4.3), malformed L2 commands (4.2), incorrect markers (4.5), empty
   fenced blocks for whole-file partners (4.4), L0-style outputs missing
   the trigger-count detail (4.6).
3. **`--documentation` does not match its specification**: it adds
   nothing at L0 and duplicates the L1 preamble at L2; it never emits
   the per-reason hints or the §12.12 command snippets that the doc
   topic blocks describe.

Validation gaps (§1.3–1.5) are independent of any of the above and
violate `<fail-closed>` regardless of what the spec resolves.
