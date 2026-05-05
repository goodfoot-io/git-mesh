# Command quirks and errors

## `git mesh commit` errors and partial failures

**`git mesh commit <name>` says nothing is staged.** Run `git mesh stale <name>` — pending staged ops appear in the trailing section. If there are none, either source code was committed without staging mesh operations, or staging was cleared by `git mesh restore <name>`. Re-stage `add` / `why` / `config` as needed, then commit.

**`git mesh commit` (no name) is best-effort across meshes.** With no positional argument — the form the post-commit hook uses — the command iterates every mesh that has non-empty staging. It prints `updated refs/meshes/v1/<name>` per success and `error: mesh <name>: <message>` per failure, continues past errors, and exits non-zero with `<n> of <m> mesh(es) failed to commit` when any fail. Successful commits are durable regardless of failures; on retry, those meshes are skipped because their staging has already drained. Address the failing meshes individually (`git mesh commit <name>`) or re-run the bare form after the cause is fixed.

## First commit requires a why

A new mesh has no parent to inherit a why from. Stage one before the first commit:

```bash
git mesh why <name> -m "Explain the relationship"
git mesh commit <name>
```

This only applies to the *first* commit of a mesh. Later commits inherit the previous why automatically.

## A staged anchor drifted from the worktree

Not an error — it's feedback. `git mesh stale` reports the staged `add` with a `drift` note when the sidecar bytes no longer match current content. Re-stage to refresh the sidecar:

```bash
git mesh add <name> 'path/to/file#L10-L20'
```

The later `add` supersedes the earlier one (last-write-wins). No `restore` or `remove` required.

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

Anchor refs point at blobs, so log traversal does not walk them as commit history regardless.

## Missing remote mesh data

```bash
git mesh fetch
```

If the remote lacks refspecs, `git mesh fetch` or `git mesh push` bootstraps them on first use. The default remote is `origin` unless `mesh.defaultRemote` is set.

## Delete refuses while staging is non-empty

`git mesh delete <name>` refuses when `.git/mesh/staging/<name>*` holds staged operations:

```
cannot delete `mymesh`: 3 staged operation(s) remain.
Run `git mesh restore mymesh` to discard them, then retry the delete.
```

This is not `WhyRequired` — the mesh already exists and has history. The refusal prevents staged residue from outliving the ref, which would cause a phantom `WhyRequired` on the next `git mesh commit`. Recovery: `git mesh restore <name>` clears staging, then `git mesh delete <name>` succeeds.

## Symlink accepted at `add` but rejected at `commit`

`git mesh add` accepts a symlink path, but `git mesh commit` rejects it:

```
error: mesh <name>: <path>: beyond a symbolic link
```

The anchor must point at the real path, not the symlink. Use `readlink -f` to resolve it:

```bash
readlink -f public/codex                # → public/claude/codex
git mesh remove <name> public/codex
git mesh add <name> public/claude/codex
```

## `git mesh doctor`

Repository-health check, not a semantic-drift check. Verifies hooks, staging files, refspecs, anchor references, dangling anchor refs, and the file index. Regenerates `.git/mesh/file-index` if missing or corrupt. Run it when local behavior looks wrong or in a developer setup step.
