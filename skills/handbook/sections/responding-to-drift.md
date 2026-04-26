# Responding to drift

A `CHANGED` or `MOVED` finding is a prompt, not a verdict. First decide whether the relationship the mesh describes still holds.

## Decide before you act

Based on the change:
- **Bytes changed and the relationship still holds (refactor, rename, formatting)**: Re-anchor. The why is inherited; do not rewrite it.
- **Bytes changed and the relationship is broken on one side**: Fix the partner range (code / test / doc), re-anchor after. Both sides should land in the same commit.
- **`MOVED` with identical bytes**: Usually leave it — the anchor follows. Re-anchor only if the new location is the one the mesh should point at going forward.
- **The relationship itself changed (different partners, different contract, new owner, new review trigger)**: Stage a new `git mesh why`, then re-anchor.
- **The relationship no longer exists**: `git mesh delete <name>`, or `git mesh revert <name> <commit-ish>` to restore a prior correct state.

## Re-anchoring

**Same `(path, extent)`, bytes changed.** A second `git mesh add` over the identical span is a re-anchor (last-write-wins). The staged add's sidecar captures current bytes, which shows `(ack)` in stale output. No `rm` required.

```bash
git mesh add <name> server/routes.ts#L13-L34
git mesh commit <name>
```

**Different line span — the range moved.** A span that does not exactly match an existing range is treated as *new*. Remove the old first:

```bash
git mesh rm  <name> server/routes.ts#L13-L34
git mesh add <name> server/routes.ts#L15-L36
git mesh commit <name>
```

This is the only time `git mesh rm` appears in a re-anchor workflow. Otherwise, `rm` only removes a range from the mesh entirely.

## The why is the relationship, not a changelog

Mesh commits inherit the previous why when none is staged. Routine re-anchors (range moved, file renamed, lines shifted) carry the relationship description forward unchanged. Stage a new why **only when the relationship itself changes**. Write it as a durable answer to "what relationship does this mesh represent?" — not a commit-log entry.

Whys are prose, not log entries: no `contract:`, `spec:`, `gov:` prefixes — the mesh name carries the label. Whys that don't repeat filenames survive `git mesh add` re-anchors after a rename without needing a rewrite (the ranges update; the prose stays correct because it talks about "the doc" or "the parser," not `docs/api/charge.md`).

```bash
git mesh why <name> -m "Token verification depends on signature verification

Owner: team-auth
Review when either signing or verification algorithm changes."
git mesh commit <name>
```

## Prose meshes drift more often than code

Prose anchors (ADRs, contracts, runbooks, API docs) churn for editorial reasons that don't change meaning: prettier or dprint reflow, heading renumbers, sentence rewrites, link sweeps. The current drift detector is line-range + blob-OID; it has no sense of "the meaning is preserved." Expect prose meshes to surface `CHANGED` more often than code meshes.

Defaults for prose meshes:
- **Whole-file pin** when the document is consumed as a unit (license, one-page ADR, published RFC). `CHANGED` then means "the bytes of this document are not what they were when you pinned it" — a real prompt to reread.
- **Line range** only when the doc has stable structural anchors (numbered ADRs, contract clauses, threat-model items with stable IDs) and the team accepts re-anchoring on editorial passes.
- **`ignore-whitespace true`** is usually right for prose — Markdown reflow is whitespace-shaped within a paragraph. It does not help when reflow moves lines across paragraphs.

When a prose `CHANGED` finding fires:
- **Editorial-only change, relationship still holds** → re-anchor (same `git mesh add` over the new span). The why carries forward unchanged.
- **The doc says something different now** → fix the partner (or stage a new why) before re-anchoring. Both sides land in the same commit, same as code.
- **The doc is being rewritten wholesale** → consider whether the relationship survives the rewrite. If it does, re-anchor at the new ranges with the unchanged why; if it does not, stage a new why or delete the mesh.

## Resolver config (mesh-level)

Copy-detection and whitespace policy are mesh state; they affect every future finding. Staged and committed with the mesh, so the team shares the same behavior.

```bash
git mesh config <name> copy-detection any-file-in-commit
git mesh config <name> ignore-whitespace true
git mesh commit <name>
```

Copy-detection values:
- **`off`** — strict rename-only or no copy tracking.
- **`same-commit`** — default; good balance for ordinary refactors.
- **`any-file-in-commit`** — code may be copied from another file touched in the same commit.
- **`any-file-in-repo`** — last resort; broad and can be expensive.

`ignore-whitespace true` is appropriate for formatting churn; it is wrong if whitespace is semantically meaningful.

## Clearing, renaming, deleting, reverting

- **`git mesh restore <name>`** — Clear `.git/mesh/staging/<name>*`. Does not touch committed history. Use when staged work should be thrown away.
- **`git mesh mv <old> <new>`** — Rename a mesh. The relationship is the same; the label is better.
- **`git mesh delete <name>`** — Remove the mesh. Use only when the relationship itself should no longer exist.
- **`git mesh revert <name> <commit-ish>`** — Restore a prior mesh state by writing a new commit. Prefer over delete when a past state was correct and history should show the restoration.

## Acknowledging drift without fixing it

Staging `git mesh add <name> <range>` after seeing drift captures current bytes as a sidecar and the next stale shows `(ack)`. Use this when the update is queued and you want a clean CI exit code, not when you're papering over a real divergence. If live content drifts from the sidecar again, the acknowledgment is invalidated.

Only mesh-layer actions acknowledge mesh drift. `git add` and `git commit` do not.
