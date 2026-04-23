---
name: git-mesh
description: Use when recording, reviewing, or auditing durable relationships between exact line ranges across a Git repository — e.g. a client call and its server handler, a schema and its consumers, a feature flag and its interpreters, or docs and the code they describe. Invoke when the user asks to "link", "mesh", "connect", or "track drift between" specific regions of code, or mentions `git mesh` / `git-mesh`.
---

# git-mesh

`git-mesh` attaches durable, reviewable metadata to exact line ranges in a
Git repository. A **range** anchors `path#Lstart-Lend` at one commit; a
**mesh** is a named, mutable set of ranges plus a message explaining why
they belong together. Data is stored in Git refs (`refs/meshes/v1/*`,
`refs/ranges/v1/*`) so it fetches, pushes, and audits like any Git
history — no side database.

## When to use

Reach for git-mesh when two or more regions must continue to agree but
aren't mechanically tied:

- client request construction ↔ server handler
- schema ↔ generated or hand-written consumers
- feature flag declaration ↔ interpreters
- public API ↔ documentation
- permissions rule ↔ tests proving it
- migration ↔ validation ↔ rollback
- invariant reimplemented in another language

Do **not** use git-mesh for relationships already enforced by the type
system, the build graph, a single file, or a single test.

## Core workflow

git-mesh mirrors Git's own habits: stage, commit, inspect, fetch, push.

```bash
# sanity check the repo and installed binary
git mesh doctor

# stage a new mesh linking two ranges
git mesh add frontend-backend-sync \
  src/client.ts#L10-L40 \
  src/server.ts#L20-L64

# attach a message explaining the relationship
git mesh message frontend-backend-sync \
  -m "Client request shape must match server handler"

# commit the staged mesh into refs/meshes/v1/*
git mesh commit frontend-backend-sync

# inspect
git mesh show frontend-backend-sync
git mesh stale frontend-backend-sync     # is each anchored range still fresh at HEAD?

# collaborate
git mesh fetch
git mesh push
```

## Agent guidance

1. Before creating a mesh, confirm the ranges exist and the line numbers
   are current — meshes anchor to an exact commit, so stale inputs
   produce stale meshes.
2. Prefer narrow ranges (the smallest region that expresses the
   contract) over whole-file ranges. Narrow ranges produce actionable
   stale-checks.
3. Always give a mesh a name that reads as a contract
   (`frontend-backend-sync`, `auth-rule-tests`) not a description of the
   change that created it.
4. `git mesh stale` is the review primitive. Run it before suggesting
   the user commit changes that touch meshed regions.
5. Mesh writes are Git commits. Treat `git mesh commit` with the same
   care as `git commit` — don't batch unrelated meshes into one
   operation.

## Installation

```bash
# via npm (wraps the platform binary)
npm install -g @goodfoot/git-mesh

# verify
git mesh doctor
```

The VS Code extension `goodfoot.git-mesh` manages the same binary and
exposes command entry points inside the editor.

## Further reading

- Handbook: `docs/git-mesh-the-missing-handbook.md`
- CLI source: `packages/git-mesh/`
- Extension: `packages/extension/`
