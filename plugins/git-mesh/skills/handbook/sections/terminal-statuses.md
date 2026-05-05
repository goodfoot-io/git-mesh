# Terminal statuses

Terminal statuses short-circuit the resolver — no slice math, no diff. Each one has a specific cause and a specific fix.

## `ORPHANED`

**Cause.** The anchor commit or anchor data is unreachable from current refs. Usually a force-push that rewrote history, a `git gc` that pruned an unreferenced commit, or a partial clone that never fetched the anchor.

**First, try to recover the missing history** — a missing fetch is the only fix that doesn't require human judgment:

```bash
git fetch --all
git mesh fetch
git mesh stale <name>     # re-check
```

**If the anchor is still ORPHANED, the goal is not to dig up the lost commit. It is to confirm the relationship still holds and re-anchor at current bytes.** Do not script a bulk "add every anchor as-is" loop over `git mesh list --porcelain`; that erases the prompt without doing the work the prompt exists for. Each mesh needs its own decision.

A user instruction like *"just re-add the anchors"* or *"skip recovery"* removes the fetch/recovery step — it does **not** remove the per-mesh confirmation below. If the shorthand sounds like a license to batch, that is the moment to slow down and surface the conflict, not the moment to script the loop.

**Per-mesh process.** For each ORPHANED mesh:

1. **Read the why.** What relationship does this mesh claim?
   ```bash
   git mesh why <name>
   ```
2. **Inspect each current anchor and write the relationship in one sentence.** Open the file at the recorded path and line range with `Read` (whole file, for whole-file anchors). The recorded line range may no longer correspond to the same content the mesh was originally pinned to — the orphan status means that history is unverifiable from refs. After reading, state in one sentence what relationship the *current* bytes at those anchors form. If you cannot write that sentence, you have not confirmed; do not re-anchor. Either inspect further, or `git mesh delete <name>`.
   ```bash
   git mesh <name> --oneline           # list anchors compactly
   # then read each path / range in the editor or via Read
   ```
   `git mesh stale <name> --patch` is the right tool for `CHANGED` anchors but produces nothing useful for true orphans (there are no anchored bytes to diff against). Read the live content directly.
3. **Decide per the drift rules in `./responding-to-drift.md`:**
   - Relationship still holds at the current location → re-anchor at the current span (`git mesh add <name> '<path>#L<start>-L<end>'`).
   - Lines have shifted → re-anchor at the *new* span (`git mesh remove` the old line range, then `git mesh add` the new one). Do **not** copy the old range forward without checking it still points at the right code.
   - The related code has diverged → fix it first, then re-anchor. Both sides land in the same commit.
   - The subsystem itself changed → stage a new why (`git mesh why <name> -m "..."`), then re-anchor.
   - The relationship no longer exists → `git mesh delete <name>`.
4. **Commit per mesh**, not in bulk:
   ```bash
   git mesh commit <name>
   ```

Bulk loops that re-add every recorded anchor verbatim are an anti-pattern: they convert "this needs review" into a clean exit code without anyone confirming the line ranges still bound the right code, that the why still describes the relationship, or that the mesh shouldn't be deleted.

## `MERGE_CONFLICT`

**Cause.** The path has no stage-0 index entry because a merge is in progress and this file is unresolved.

**Fix.** Finish the merge (resolve conflicts, `git add`, `git commit`). Run `git mesh stale` again.

## `SUBMODULE`

**Cause.** A legacy anchor points *inside* a submodule. git-mesh does not open submodules to compare line ranges.

**Fix.** Remove the anchor and re-pin at the appropriate level:
```bash
git mesh remove <name> '<submodule-path>/inner/file.ts#L10-L20'
# Either: whole-file pin on the submodule root
git mesh add <name> <submodule-path>
# Or: pin a parent-repo path that witnesses the same relationship
git mesh commit <name>
```

Whole-file anchors on a submodule *root* (the gitlink path) are supported — the resolver compares gitlink SHAs without opening the submodule.

## `SidecarTampered`

**Cause.** A staged `add` op's sidecar bytes under `.git/mesh/staging/<name>.<N>` no longer match the SHA-256 recorded in its `.meta` file. Either the sidecar was edited out-of-band, or the staging area was corrupted.

**Fix.** Fail-closed — `git mesh commit`, `git mesh stale`, and `git mesh doctor` all surface this. Clear and re-stage:
```bash
git mesh restore <name>
git mesh add <name> <anchor>
git mesh commit <name>
```

If `doctor` reports it without an obvious staging cause, investigate whether something writes into `.git/mesh/staging/` (misconfigured hook, sync tool, manual edit).
