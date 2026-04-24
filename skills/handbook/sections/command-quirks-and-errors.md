# Command quirks and errors

## `git mesh commit` says nothing is staged

Run `git mesh stale <name>` — pending staged ops appear in the trailing section. If there are none, either source code was committed without staging mesh operations, or staging was cleared by `git mesh restore <name>`. Re-stage `add` / `why` / `config` as needed, then commit.

## First commit requires a why

A new mesh has no parent to inherit a why from. Set one:

```bash
git mesh why <name> -m "Explain the relationship"
git mesh commit <name>
```

This only applies to the *first* commit of a mesh. Later commits inherit the previous why automatically.

## A staged range drifted from the worktree

Not an error — it's feedback. `git mesh stale` reports the staged `add` with a `drift` note when the sidecar bytes no longer match current content. Re-stage to refresh the sidecar:

```bash
git mesh add <name> path/to/file#L10-L20
```

The later `add` supersedes the earlier one (last-write-wins). No `restore` or `rm` required.

## Re-anchoring — same extent vs new span

Same `(path, extent)` with new bytes: just `git mesh add` again. Different line span: `git mesh rm` old, `git mesh add` new. Overlapping but non-identical ranges (e.g. `#L1-L10` and `#L5-L15`) are allowed and coexist.

## `SidecarTampered` in `doctor` or `stale`

Fail-closed — sidecar bytes no longer match the recorded SHA-256. See `./terminal-statuses.md`.

## `git log --all` shows mesh commits

Mesh commits live under custom refs (`refs/meshes/v1/*`), so all-ref traversals see them. Prefer a positively-scoped traversal that never expands the custom namespace:

```bash
git log --branches --remotes --tags
git config alias.hist 'log --graph --branches --remotes --tags'
```

`--exclude=refs/meshes/*` on `git log --all` **does not work** — the flag filters only the expansion that follows it, and when `--all` precedes `--exclude` there is nothing left to filter.

To keep `--all` and only quiet decoration on mesh commits (they still appear in the walk):

```bash
git config log.excludeDecoration refs/meshes/*
```

Range refs point at blobs, so log traversal does not walk them as commit history regardless.

## Missing remote mesh data

```bash
git mesh fetch
```

If the remote lacks refspecs, `git mesh fetch` or `git mesh push` bootstraps them on first use. The default remote is `origin` unless `mesh.defaultRemote` is set.

## `git mesh doctor`

Repository-health check, not a semantic-drift check. Verifies hooks, staging files, refspecs, range references, dangling range refs, and the file index. Regenerates `.git/mesh/file-index` if missing or corrupt. Run it when local behavior looks wrong or in a developer setup step.
