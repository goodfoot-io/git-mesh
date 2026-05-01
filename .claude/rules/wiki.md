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
3. Run `wiki scaffold <page>` to propose covering meshes for line-ranged fragment links.
4. Consolidate the scaffold output into meaningful per-source-file (or per-subsystem) meshes rather than the per-section split it suggests. Write meaningful `why` text — not `[why]`.
5. `git mesh add` requires every anchored path to exist in HEAD. If the wiki page is one of the anchors (it usually is), the page must be committed before `git mesh add`. The wiki page itself does **not** need to be committed for `wiki check` wikilink resolution — `repo_inventory` unions tracked paths with untracked-Added items.
6. `git mesh commit` each new mesh.
7. Run `wiki check <page>` — should exit clean.

## `wiki check` failure modes

In the order they typically surface:

- **`missing_sha`** — fragment link has no pinned `&<sha>`. Fix: `wiki check --fix` auto-pins from git history. Never hand-edit SHAs.
- **`broken_wikilink`** — no page has the given title or alias. Likely causes: frontmatter `title`/`aliases` does not match the link text (resolution is case-insensitive but exact otherwise), the target page lives outside the discovered wiki roots, or the target path is gitignored. HEAD presence is **not** required — `repo_inventory` includes untracked files. If a fresh `wiki list` shows the page but `wiki check` does not, suspect a stale `wiki/.index.db` or a binary-version mismatch.
- **`mesh_uncovered`** — every fragment link with a line range (`#L<start>-L<end>`) must be covered by a `git mesh`. Whole-file links do not require coverage. Fix: `wiki scaffold` then create + commit covering meshes.

## Disk hygiene

`wiki/` accumulates runtime artifacts that must not be committed: `.index.db`, `.index.db-wal`, `wiki.log`. Add a `wiki/.gitignore` excluding them when first setting up. `wiki.toml` may be empty.

## Mesh anchoring requirement

`git mesh add` rejects anchors whose paths do not exist in HEAD. This applies to the wiki page itself when it is one of the anchors — commit the page before staging meshes that reference it. (This is a `git mesh` constraint, not a `wiki check` constraint.)
