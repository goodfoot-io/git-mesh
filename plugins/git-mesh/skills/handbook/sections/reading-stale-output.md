# Reading `git mesh stale` output

`git mesh stale` asks: *do the anchored bytes still match reality?* It prints one finding per range per drifting layer, plus pending entries for any staged mesh ops.

## Status values

- **`FRESH`** — Current bytes equal anchored bytes at the same location. No action.
- **`MOVED`** — Bytes are equal, but path or line numbers changed. Usually keep; re-anchor only if the new location is the one the mesh should point at.
- **`CHANGED`** — Current bytes differ, or the range was deleted. Review the relationship, then update code or mesh.
- **`ORPHANED`** / **`MERGE_CONFLICT`** / **`SUBMODULE`** / **`CONTENT_UNAVAILABLE(...)`** — Terminal. Read `./terminal-statuses.md` or `./content-unavailable.md`.

## Layers and the `src` column

The resolver checks up to four layers, shallowing order: HEAD → Index → Worktree → staged mesh ops. Each drifting layer produces its own finding.

- **`src=H`** — Drift is already in HEAD (committed).
- **`src=I`** — Drift is in the index (staged with `git add`) but not HEAD.
- **`src=W`** — Drift is in the worktree (unstaged edit on disk).
- **Staged mesh ops** render in a trailing section, not in the main finding list.

The same range can appear twice when two layers both differ — e.g. a file with one edit `git add`-ed (src=I) and another edit left unstaged (src=W). That's the layering doing its job, not a duplicate.

Peel layers with subtractive flags:
- `--no-worktree` drops W findings.
- `--no-index` drops I findings.
- `--no-staged-mesh` drops the staged-ops section.
- HEAD is always on — no flag turns it off.

## `(ack)` — acknowledgment

A finding prints `(ack)` when a staged `git mesh add` covers it. Staging an `add` after seeing drift captures current bytes as a sidecar; the resolver treats that as "I've seen this, the update is queued" — the finding still prints, but exit code is not driven by it. The moment live content drifts from the sidecar again, the acknowledgment is invalidated.

Only mesh-layer actions acknowledge mesh drift. `git add` and `git commit` move file drift between layers; they do not silence findings.

## Exit code

Non-zero if any of:
- A finding has drift at HEAD / Index / Worktree with no matching staged re-anchor.
- A terminal status (`ORPHANED`, `MERGE_CONFLICT`, `SUBMODULE`, `CONTENT_UNAVAILABLE`) isn't suppressed.
- A staged `add`/`remove` op has a sidecar whose bytes disagree with the blob it anchors.

`--no-exit-code` forces exit 0 regardless of findings. `--ignore-unavailable` downgrades `CONTENT_UNAVAILABLE` only.

## Machine formats

```bash
git mesh stale --format=porcelain
git mesh stale --format=json
git mesh stale --format=junit
git mesh stale --format=github-actions
```

JSON carries `{ "schema_version": 1, ... }`. Parse the `source` field for the layer (`(index)` / `(worktree)` / `(staged)`); do not read the culprit column as a SHA. `CONTENT_UNAVAILABLE` findings carry a `reason`.

## Plugin `additionalContent` blocks

The plugin emits drift summaries that look like:

```
request-schema mesh: Charge request schema is shared by client and server.
- src/api.ts#L1-L3 [CHANGED]
- server/routes.ts#L8-L21
```

The first line is the mesh name + current why. Each bullet is a range; the marker in brackets is its status. Absence of a marker means `FRESH`. Use the why line to decide whether the change is intentional before touching either side.
