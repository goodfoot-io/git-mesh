# git mesh

A mesh names an implicit semantic dependency between two anchors — in code or prose — that is real, load-bearing, and not enforced by any type, schema, or test. It exists so a future developer touching one anchor learns, at that moment, what the other anchor relies on them to keep true. The standing question whenever you edit, read, or review is: does this region create or rely on a coupling that isn't visible from the lines themselves? If yes, and if you can name a concrete wrong decision someone would make under deadline when the related anchor changes silently, write a mesh.

```bash
# Stage the mesh: slug names the relationship, the whole-file anchor pins the prose spec, the line-range anchor pins the exact parser bytes.
git mesh add billing/charge-request-contract \
  docs/api/charge.md \
  services/api/charge.ts#L30-L76

# One prose sentence in role-words; names which side is normative. Every new mesh needs a why before it can be committed.
git mesh why billing/charge-request-contract \
  -m "The published request spec states the body shape the parser honors; the spec is the source of truth when they disagree."

# The post-commit hook runs `git mesh commit`, binding the staged anchors to the commit that just landed.
git commit -m "Document charge request contract and wire the parser"
```

When you read or edit a file that participates in a mesh, you'll see a surface block naming the related anchor, the why, and a status marker. `FRESH` means the related anchor still matches its anchored bytes; `CHANGED` means the related anchor has drifted and you must open it and reconcile before continuing — either align your edit with it, update it in the same change, or re-anchor and update the why if the relationship itself has shifted. Run `git mesh stale` on demand before committing, after conflicts or large moves, or when planning a refactor that spans many anchors, to see the full drift landscape across HEAD, index, and worktree. A surfaced `CHANGED` is not optional reading.

```text
$ git mesh stale
mesh billing/charge-request-contract

1 stale of 2 anchors:

  CHANGED services/api/charge.ts#L30-L76
```

The finding names the mesh and points at the exact anchor that drifted. Pass `--show-src` to see which layer the drift is in (`H` HEAD, `I` index, `W` worktree). Open the related anchor (`docs/api/charge.md`) to read the why and decide whether the spec needs to follow the parser or the parser needs to be brought back in line — then either update the prose in the same commit or re-anchor with `git mesh add` if the relationship itself has shifted. The non-zero exit code blocks the commit until reconciled.
