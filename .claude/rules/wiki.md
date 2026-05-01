---
paths:
  - "**/*.wiki.md"
  - "wiki/**/*.md"
---

# Wiki Page Authoring

## Workflow shape

End-to-end loop for creating or updating a wiki page:

1. Write or edit the page (frontmatter with `title` + `summary`, fragment links to source).
2. Run `wiki check --fix <page>` to auto-pin fragment-link SHAs.
3. **Commit the page** — `wiki check` resolves wikilinks and `git mesh add` anchors against HEAD, not the working tree. Skipping this step is the most common cause of false-positive failures below.
4. Run `wiki scaffold <page>` to propose covering meshes for line-ranged fragment links.
5. Consolidate the scaffold output into meaningful per-source-file (or per-subsystem) meshes rather than the per-section split it suggests. Write meaningful `why` text — not `[why]`.
6. `git mesh commit` each new mesh.
7. Run `wiki check <page>` — should exit clean.

## `wiki check` failure modes

In the order they typically surface:

- **`missing_sha`** — fragment link has no pinned `&<sha>`. Fix: `wiki check --fix` auto-pins from git history. Never hand-edit SHAs.
- **`broken_wikilink`** — `[[Title]]` target's file is not in HEAD. `wiki list` reads the filesystem and will mislead you here; `wiki check` only sees committed pages. Fix: commit the target page.
- **`mesh_uncovered`** — every fragment link with a line range (`#L<start>-L<end>`) must be covered by a `git mesh`. Whole-file links do not require coverage. Fix: `wiki scaffold` then create + commit covering meshes.

## Disk hygiene

`wiki/` accumulates runtime artifacts that must not be committed: `.index.db`, `.index.db-wal`, `wiki.log`. Add a `wiki/.gitignore` excluding them when first setting up. `wiki.toml` may be empty.

## Mesh anchoring requirement

`git mesh add` rejects anchors whose paths do not exist in HEAD. This applies to the wiki page itself when it is one of the anchors — commit the page before staging meshes that reference it.
