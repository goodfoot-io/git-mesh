# CI and sync

## Sync

```bash
git mesh fetch [<remote>]
git mesh push  [<remote>]
```

`git-mesh` lazily configures fetch and push refspecs for `refs/ranges/*` and `refs/meshes/*` on first use. Default remote is `origin`:

```bash
git config mesh.defaultRemote upstream   # override
```

Inspect remote mesh refs with git plumbing:

```bash
git ls-remote origin 'refs/meshes/*'
git ls-remote origin 'refs/ranges/*'
```

**Fetch before reviewing shared mesh state.** Reads are local; they do not contact the network.

## HEAD-only invariant (the CI mode)

CI runners should not see checkout noise — line-ending churn, auto-generated files, smudge-time artifacts. Collapse the resolver to its HEAD-layer floor with the three subtractive flags:

```bash
git mesh stale --no-worktree --no-index --no-staged-mesh
```

There is no convenience alias — pass all three so intent is visible.

## PR gate (scope to branch)

```bash
git mesh fetch origin
base="$(git merge-base origin/main HEAD)"
git mesh stale --since "$base" \
  --no-worktree --no-index --no-staged-mesh \
  --format=github-actions
```

`--since` limits findings to ranges anchored on the current branch. `--format=github-actions` emits annotations for GitHub Actions; `junit` and `json` are also available.

## Full repository audit (scheduled)

```bash
git mesh fetch origin
git mesh stale \
  --no-worktree --no-index --no-staged-mesh \
  --format=junit
```

Use for repositories with many relationships that can drift without a nearby PR.

## Advisory report (no gating)

```bash
git mesh stale \
  --no-worktree --no-index --no-staged-mesh \
  --no-exit-code --format=json > mesh-report.json
```

`--no-exit-code` forces exit 0 regardless of findings. Use for dashboards or migration work where stale meshes are counted, not blocked.

## Fresh-clone tolerance

On CI runners that have not fetched LFS or partial-clone content:

```bash
git mesh stale \
  --no-worktree --no-index --no-staged-mesh \
  --ignore-unavailable \
  --format=github-actions
```

`--ignore-unavailable` downgrades only `CONTENT_UNAVAILABLE` findings. Drift findings still fail. See `./content-unavailable.md` for reason codes.

## Setup audit

```bash
git mesh doctor
```

Lightweight repository-health check — suitable for developer setup or a CI pre-check. Not a semantic-drift check.
