# Inspecting meshes

Reading mesh state is local and fast — no network. Fetch first if the question is about shared state:

```bash
git mesh fetch
```

## List all meshes

One block per mesh, with why and anchors. Staged or pending meshes are marked.

```bash
git mesh list
git mesh list --porcelain          # tab-separated rows: name<TAB>path<TAB>start-end
git mesh list --search 'parser'    # filter by name, why, or anchor address (case-insensitive)
git mesh list --offset 10 --limit 10   # pagination (by mesh, after filters)
```

Bare `git mesh` (no arguments) prints short help.

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

## Find meshes touching a file or anchor

Overlap semantics — a mesh is listed if any anchor touches the queried path or range. The full anchor list of each matching mesh is always shown.

```bash
git mesh list src/Button.tsx
git mesh list src/Button.tsx#L40-L60
git mesh list src/Button.tsx src/Button.css     # multiple targets — unioned, deduped
git mesh list checkout-request-flow src/api.ts  # mesh name + path mixed
```

A target that resolves to no meshes is fine on its own — the command exits 0. The command only errors when a target names something that doesn't exist (missing file, missing mesh name, or a literal glob the shell didn't expand). The same rule applies to `git mesh stale [<target>...]`.

## Before a mesh's first commit

A mesh ref does not exist until `git mesh commit <name>` succeeds once. Before that:

- **`git mesh stale`** (no targets) — workspace scan; shows staged ops for the not-yet-committed mesh in the trailing "staged mesh ops" section.
- **`git mesh stale <new-name>`** — resolves via staging if `<new-name>` has staged ops. If `<new-name>` is neither a mesh, a path-index entry, nor a file in the worktree, errors with `no such file or mesh: '<new-name>'`.
- **`git mesh <new-name>`** — errors: mesh ref not found.
- **`git mesh list`** — pending meshes (staging-only, no committed tip) appear with a `(pending)` marker.
