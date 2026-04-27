# Terminal statuses

Terminal statuses short-circuit the resolver — no slice math, no diff. Each one has a specific cause and a specific fix.

## `ORPHANED`

**Cause.** The anchor commit or anchor data is unreachable from current refs. Usually a force-push that rewrote history, a `git gc` that pruned an unreferenced commit, or a partial clone that never fetched the anchor.

**Fix.**
```bash
git fetch --all
git mesh fetch
```
If the anchor commit is truly gone, re-anchor on a reachable commit:
```bash
git mesh add <name> <path>#L<start>-L<end>
git mesh commit <name>
```

## `MERGE_CONFLICT`

**Cause.** The path has no stage-0 index entry because a merge is in progress and this file is unresolved.

**Fix.** Finish the merge (resolve conflicts, `git add`, `git commit`). Run `git mesh stale` again.

## `SUBMODULE`

**Cause.** A legacy anchor points *inside* a submodule. git-mesh does not open submodules to compare line ranges.

**Fix.** Remove the anchor and re-pin at the appropriate level:
```bash
git mesh rm <name> <submodule-path>/inner/file.ts#L10-L20
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
