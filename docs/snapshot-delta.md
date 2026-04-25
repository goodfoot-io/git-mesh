# Workspace Snapshot and Delta Scripts

`scripts/snapshot.sh` and `scripts/delta.sh` capture and compare filesystem
state inside a Git working tree without mutating the repository checkout.

They are intended for workflows that need a stable baseline and a
machine-readable patch of everything that changed after that baseline,
including edits, renames, deletions, and untracked files.

## Usage

Create or replace a snapshot:

```sh
./scripts/snapshot.sh <id>
```

Show the delta from that snapshot to the current workspace state:

```sh
./scripts/delta.sh <id>
```

The `<id>` is an arbitrary string. Reusing the same ID in the same worktree
overwrites the previous snapshot for that ID.

Example:

```sh
./scripts/snapshot.sh before-tool-run

# Make edits, create files, rename files, delete files, stage files, etc.

./scripts/delta.sh before-tool-run > workspace.patch
```

`delta.sh` writes a standard Git patch to stdout using:

```sh
git diff --no-ext-diff --no-color --binary --full-index --find-renames
```

If no filesystem content changed since the snapshot, `delta.sh` exits
successfully with empty output.

## What Is Captured

The scripts capture filesystem content in the Git worktree:

- tracked file edits;
- tracked file deletions;
- tracked and untracked renames, as detected by `git diff --find-renames`;
- untracked files;
- binary file changes, using Git binary patches;
- executable-bit mode changes.

Ignored files are excluded using Git's normal ignore rules
(`git ls-files --others --exclude-standard`). The `.git` directory is not part
of the captured workspace state.

The scripts do not preserve the staging boundary. A snapshot records the
current file contents visible in the working tree, not whether those contents
are committed, staged, or unstaged. If a file's bytes are unchanged but its
staged state changes, `delta.sh` does not report that as a mutation.

## How It Works

Both scripts build a temporary Git tree that represents the current filesystem
state.

The tree-building process is:

1. Resolve the repository root and worktree-specific Git directory.
2. Copy the real Git index to a temporary index.
3. Run `git add -u -- .` against the temporary index to reflect tracked file
   edits and deletions.
4. List non-ignored untracked files with
   `git ls-files -z --others --exclude-standard`.
5. Add those untracked files to the temporary index using NUL-delimited
   pathspecs.
6. Run `git write-tree` against the temporary index.

Because `GIT_INDEX_FILE` points at the temporary index, the real index is not
changed. Because `GIT_OBJECT_DIRECTORY` points at temporary snapshot storage,
new blob and tree objects are not written to the repository's object database.

`snapshot.sh` stores the resulting tree ID and object directory path in
temporary state. `delta.sh` builds a new temporary tree for the current
workspace, then runs `git diff` between the stored snapshot tree and the
current tree.

## Snapshot Storage

By default, snapshots are stored under:

```text
${TMPDIR:-/tmp}/git-workspace-snapshots
```

Set `GIT_WORKSPACE_SNAPSHOT_DIR` to use a different storage location:

```sh
GIT_WORKSPACE_SNAPSHOT_DIR=/var/tmp/git-snapshots ./scripts/snapshot.sh run-1
GIT_WORKSPACE_SNAPSHOT_DIR=/var/tmp/git-snapshots ./scripts/delta.sh run-1
```

Snapshots are isolated by both the physical worktree path and the
worktree-specific Git directory. That means the same `<id>` can be reused in
different repositories or linked worktrees without collision.

Snapshot files contain only metadata: format version, tree ID, object directory
name, and creation time. File contents needed by that tree live as Git objects
inside the snapshot storage directory.

## Repository Mutation Guarantees

The scripts are designed not to mutate workspace state:

- no checked-out files are changed;
- the real Git index is not changed;
- Git refs are not created or updated;
- Git config is not changed;
- new objects are written to temporary snapshot storage, not the repository's
  `.git/objects` directory.

The scripts may create, replace, or delete files under their snapshot storage
directory. Re-running `snapshot.sh <id>` removes the previous temporary object
directory for that same repository/worktree and ID.

## Failure Behavior

The scripts use `set -euo pipefail` and fail closed. If Git cannot read or add a
file, write the temporary tree, find a snapshot, or read the stored snapshot
objects, the command exits non-zero instead of producing a partial successful
result.

Common failures:

- running outside a Git repository;
- calling `delta.sh <id>` before `snapshot.sh <id>`;
- deleting the snapshot storage directory before running `delta.sh`;
- filesystem permission errors while reading workspace files or writing
  snapshot state.

## Limitations

The patch represents the difference between two filesystem-content trees. It
does not report:

- staging-only changes where file contents are unchanged;
- ignored files;
- changes inside nested Git repositories or submodules as ordinary files.

Submodules are represented by their Gitlink entry from the parent repository,
not by recursively snapshotting the submodule's internal working tree.
