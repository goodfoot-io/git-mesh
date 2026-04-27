# Inspecting meshes

Reading mesh state is local and fast — no network. Fetch first if the question is about shared state:

```bash
git mesh fetch
```

## List all meshes

One line per mesh: name, tip commit, range count.

```bash
git mesh
```

## Show a single mesh

```bash
git mesh <name>                   # full view
git mesh <name> --oneline         # compact
git mesh <name> --no-abbrev       # full SHAs
```

Print the current why:

```bash
git mesh why <name>
```

## Historical state

`--at` accepts any commit-ish git understands:

```bash
git mesh <name> --at HEAD~3
git mesh <name> --at <mesh-ref-sha>
git mesh why <name> --at HEAD~5
```

Resolution rules:
- **Source commit-ish** (branch, tag, `HEAD~N`) — resolves to the mesh state current when that source commit was HEAD.
- **Mesh-ref commit SHA** — used as-is.

## Walk mesh history

```bash
git mesh <name> --log
git mesh <name> --log --limit 5
```

## Format for scripts

```bash
git mesh <name> --format='%h %s%n%(ranges)'
git mesh <name> --format='%(ranges:count)'
git mesh <name> --format='%(config:copy-detection)'
```

## Find meshes touching a file or range

Overlap semantics — a mesh range appears if it touches any queried line.

```bash
git mesh ls
git mesh ls src/Button.tsx
git mesh ls src/Button.tsx#L40-L60
```

## Before a mesh's first commit

A mesh ref does not exist until `git mesh commit <name>` succeeds once. Before that:

- **`git mesh stale`** (no name) — workspace scan; shows staged ops for the not-yet-committed mesh in the trailing "staged mesh ops" section.
- **`git mesh stale <new-name>`** — errors: mesh ref not found.
- **`git mesh <new-name>`** — errors: mesh ref not found.
- **`git mesh ls`** — use to confirm which ranges are staged so far.
