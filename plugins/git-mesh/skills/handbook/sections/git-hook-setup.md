# Git hook setup

`git mesh` integrates with two repository-local git hooks:

- **`post-commit`** — promotes staged mesh edits and reconciles drift after
  every commit.
- **`post-rewrite`** — re-anchors meshes after a rebase or
  `git commit --amend`.

Install both at the repository root:

```sh
mkdir -p .git/hooks

cat > .git/hooks/post-commit <<'EOF'
#!/bin/sh
if command -v git-mesh >/dev/null 2>&1; then
  git mesh commit
  git mesh stale --compact --auto-follow --no-exit-code
fi
EOF

cat > .git/hooks/post-rewrite <<'EOF'
#!/bin/sh
if command -v git-mesh >/dev/null 2>&1; then
  git mesh rewrite
  git mesh stale --compact --auto-follow --no-exit-code
fi
EOF

chmod +x .git/hooks/post-commit .git/hooks/post-rewrite
```

What each line does:

- **`git mesh commit`**: promotes staged mesh edits into `refs/meshes/v1/*`
  as part of the same commit moment.
- **`git mesh stale --compact --auto-follow --no-exit-code`** (post-commit):
  advances Fresh anchors to HEAD when the anchored bytes are unchanged, and
  follows renames so anchors track moved files. `--no-exit-code` keeps drift
  findings from failing the commit.
- **`git mesh rewrite`**: reads the old→new SHA pairs git provides on stdin
  and advances anchor SHAs whose anchored bytes are unchanged across the
  rewrite.
- **`git mesh stale --compact --auto-follow --no-exit-code`**
  (post-rewrite): backstop for multi-step rewrites (e.g.
  `rebase --exec git commit --amend`) that can leave an anchor on an
  unreachable intermediate. `--compact` advances such anchors to HEAD when
  the path's HEAD blob still byte-equals the anchored blob, and
  `--auto-follow` picks up renames that happened across the rewrite.

The `command -v git-mesh` guard makes both hooks safe in repos or
environments where the CLI isn't installed.

`git mesh doctor` reports `MissingPostCommitHook` /
`MissingPostRewriteHook` until each hook file contains the marker line
(`git mesh commit` and `git mesh rewrite` respectively); both findings
should drop off after the snippets above are in place.

## Wrapping an existing hook

If a hook file already exists (e.g. another tool installed one), append the
`git mesh` lines rather than overwriting. Doctor only checks that the
marker text appears anywhere in the hook file, so any wrapper that
ultimately runs the command will satisfy it.
