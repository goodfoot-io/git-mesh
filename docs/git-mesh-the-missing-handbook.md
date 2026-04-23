# git-mesh: The missing handbook

`git-mesh` lets you attach durable, reviewable metadata to exact line
ranges in a Git repository. It is for relationships that Git can store
but does not naturally explain: "this client call must stay compatible
with this server handler," "these generated types reflect this schema,"
"this documentation section describes this code," or "these files must
change together."

The important mental model is simple:

- A **range** is an immutable anchor to `path#Lstart-Lend` at one commit.
- A **mesh** is a named, mutable set of ranges plus a message explaining
  why those ranges belong together.
- A **stale check** recomputes whether each anchored range is still
  fresh at `HEAD`.

The tool deliberately follows Git's own habits: stage first, commit
second, inspect locally, fetch explicitly, push intentionally, and let
history carry the audit trail.

## Why git-mesh exists

Git is excellent at recording what changed. It is less good at recording
which separate pieces of code must continue to agree after they drift
through unrelated commits. Code review can catch that relationship once.
Comments can describe it, but comments live in one file and drift with
the file. Issue trackers can describe it, but they are outside the repo.

`git-mesh` stores that relationship in Git itself:

- Mesh data lives in custom refs under `refs/meshes/v1/*`.
- Range data lives in custom refs under `refs/ranges/v1/*`.
- Mesh edits are Git commits with parents, authors, dates, messages, and
  trees.
- Ranges are Git blobs, so anchored records are immutable objects.
- Reads do not hit the network; they use the local object database.

This design follows Git's data model rather than fighting it. Git's own
documentation describes the repository as objects, references, the index,
and reflogs; it also notes that tools may create references under custom
`refs/` namespaces. `git-mesh` uses that extension point rather than a
side database.

## What to mesh

Mesh things that are related by meaning, contract, or maintenance
responsibility but are not already mechanically tied together.

Good candidates:

- Request construction in a client and request parsing in a server.
- A schema definition and generated or handwritten consumers.
- A feature flag declaration and the places that interpret the flag.
- A permissions rule and tests that prove the rule.
- A public API and documentation that promises its behavior.
- A migration, validation rule, and rollback code.
- Two implementations of the same invariant in different languages.

Poor candidates:

- Whole files where a smaller range would explain the relationship.
- Temporary work-in-progress notes that should be a normal commit or PR
  comment.
- Relationships generated perfectly by a build step.
- Anything that should be enforced by a compiler, type checker, schema
  validator, or test instead.

Use `git-mesh` as a memory and review tool, not as a substitute for
automation. If a relationship can be made impossible to break, do that
first. Mesh what still needs human attention.

## The Git habits git-mesh inherits

The best Git and version-control guidance transfers directly:

- Keep integration frequent. Trunk-based development guides emphasize
  short-lived branches and continuous integration because stale branches
  hide conflicts. Meshes benefit from the same discipline: stale checks
  are more useful when drift is found soon.
- Review through pull requests. GitHub Flow and feature-branch guides
  center code review around branch diffs. Mesh changes should travel
  with the source changes that make the relationship true.
- Make commits coherent. A mesh commit should explain one relationship
  or one relationship update. Do not use one mesh as a junk drawer.
- Prefer explicit history to mutation. Git, Mercurial, and Subversion
  all teach that branching/tagging/history are coordination tools.
  `git-mesh revert` restores a previous mesh state by writing a new
  commit rather than erasing history.
- Treat local state as local. Git's index and working tree are not
  shared until committed and pushed. `git-mesh` staging files work the
  same way.
- Fetch before judging remote truth. Local reads are fast and reliable,
  but they are local. Run `git mesh fetch` before reviewing shared mesh
  state.

## Installation and first check

From a repository that has `git-mesh` available:

```bash
git mesh doctor
```

`doctor` checks the local mesh setup: hooks, staging files, refspecs,
range references, dangling range refs, and the file index. It also
regenerates a missing or corrupt `.git/mesh/file-index`.

For a team repository, install these hooks or their managed equivalents:

```bash
# .git/hooks/pre-commit
#!/bin/sh
git mesh status --check
```

```bash
# .git/hooks/post-commit
#!/bin/sh
git mesh commit
```

The pre-commit hook catches staged mesh ranges whose working-tree bytes
changed after `git mesh add`. The post-commit hook lets you stage mesh
updates before a source commit and anchor them to the commit that just
landed.

## A first mesh

Suppose a UI component posts a shape that must match a server route:

```bash
git mesh add frontend-backend-sync \
  src/Button.tsx#L42-L50 \
  server/routes.ts#L13-L34

git mesh message frontend-backend-sync -m "ABC-123: Button charge request matches charge route

Owner: team-billing
Review when either request body or response shape changes."

git mesh commit frontend-backend-sync
```

Then inspect it:

```bash
git mesh frontend-backend-sync
```

Later, check whether the relationship drifted:

```bash
git mesh stale frontend-backend-sync
```

The line range syntax matches GitHub's URL fragment style:

```text
path/to/file.ext#L12-L20
```

Ranges are 1-based and inclusive.

## The daily workflow

### Creating a mesh while changing code

1. Make the code change.
2. Stage the mesh ranges with `git mesh add`.
3. Write the mesh message with `git mesh message`.
4. Commit the source change with `git commit`.
5. Let the post-commit hook run `git mesh commit`, or run it manually.

Example:

```bash
git mesh add billing-contract \
  web/checkout.tsx#L88-L120 \
  api/charge.ts#L30-L76

git mesh message billing-contract -m "Checkout request matches charge API contract"
git status
git mesh status billing-contract
git commit -m "Wire checkout to charge API"
git mesh status billing-contract
```

`git mesh add` without `--at` snapshots the working tree and resolves the
anchor at mesh commit time. This is what makes the post-commit flow work:
the range anchors to the source commit that just landed.

### Creating a mesh for existing code

When documenting a relationship that already exists in history, anchor
explicitly:

```bash
git mesh add auth-token-contract --at HEAD \
  packages/auth/token.ts#L88-L104 \
  packages/auth/crypto.ts#L12-L40

git mesh message auth-token-contract -m "Token verification depends on signature verification"
git mesh commit auth-token-contract
```

Use `--at <commit-ish>` when you want the anchor to be a specific
historical commit rather than the current post-commit hook moment.

### Updating a mesh after drift

Run stale:

```bash
git mesh stale frontend-backend-sync --patch
```

If a range moved but the bytes are identical, inspect the movement and
decide whether the existing mesh still tells the truth. If the bytes
changed, review the partner ranges and update code, tests, docs, or the
mesh.

To re-anchor a changed range:

```bash
git mesh rm frontend-backend-sync server/routes.ts#L13-L34
git mesh add frontend-backend-sync server/routes.ts#L15-L36
git mesh message frontend-backend-sync -m "Re-anchor route after session helper extraction"
git mesh commit frontend-backend-sync
```

Remove then add is the intended re-anchor workflow. The tool rejects two
active ranges with the same `(path, start, end)` inside one mesh.

### Changing only the message

```bash
git mesh message frontend-backend-sync -m "ABC-123: Button charge request matches charge route"
git mesh commit frontend-backend-sync
```

Message-only commits are useful when the relationship is right but the
explanation is weak. Write messages as if a reviewer will see them six
months from now.

### Changing resolver settings

```bash
git mesh config frontend-backend-sync copy-detection any-file-in-commit
git mesh config frontend-backend-sync ignore-whitespace true
git mesh commit frontend-backend-sync
```

Resolver settings are mesh-level state. They are staged and committed
with the mesh, so the team shares the same behavior.

Use `ignore-whitespace true` sparingly. It is appropriate for formatting
churn; it is wrong if whitespace is semantically meaningful.

Copy detection choices:

| Value | Use when |
|---|---|
| `off` | You want strict rename-only or no copy tracking. |
| `same-commit` | Default; good balance for ordinary refactors. |
| `any-file-in-commit` | Code may be copied from another file touched in the same commit. |
| `any-file-in-repo` | Last resort for broad copy detection; can be expensive. |

### Clearing staged mesh work

```bash
git mesh restore frontend-backend-sync
```

This clears `.git/mesh/staging/frontend-backend-sync*`. It does not
change committed mesh history.

### Renaming, deleting, and reverting

```bash
git mesh mv old-name new-name
git mesh delete obsolete-mesh
git mesh revert frontend-backend-sync <mesh-commit-ish>
```

Prefer `revert` over delete when a past state was correct and you want
history to show the restoration. Delete only when the relationship
itself should no longer exist.

## Reading meshes

List meshes:

```bash
git mesh
```

Show one mesh:

```bash
git mesh frontend-backend-sync
git mesh frontend-backend-sync --oneline
git mesh frontend-backend-sync --no-abbrev
```

Show a historical state:

```bash
git mesh frontend-backend-sync --at HEAD~3
```

Walk mesh history:

```bash
git mesh frontend-backend-sync --log
git mesh frontend-backend-sync --log --limit 5
```

Format output for scripts:

```bash
git mesh frontend-backend-sync --format='%h %s%n%(ranges)'
git mesh frontend-backend-sync --format='%(ranges:count)'
git mesh frontend-backend-sync --format='%(config:copy-detection)'
```

Find meshes touching a file:

```bash
git mesh ls
git mesh ls src/Button.tsx
git mesh ls src/Button.tsx#L40-L60
```

The range form uses overlap semantics. A mesh range appears if it touches
any queried line.

## Understanding stale output

`git mesh stale` is the command that asks: "Do the anchored bytes still
match the current tree?"

```bash
git mesh stale
git mesh stale frontend-backend-sync
git mesh stale frontend-backend-sync --oneline
git mesh stale frontend-backend-sync --stat
git mesh stale frontend-backend-sync --patch
```

Status values:

| Status | Meaning | Typical response |
|---|---|---|
| `FRESH` | Current bytes equal anchored bytes at the same location. | No action. |
| `MOVED` | Bytes are equal, but path or line numbers changed. | Inspect and usually keep or re-anchor. |
| `CHANGED` | Current bytes differ, or the range was deleted. | Review the relationship and update code or mesh. |
| `ORPHANED` | The anchor commit or range data is unreachable. | Fetch, investigate force-push/gc, or re-anchor. |

By default, any non-`FRESH` finding exits non-zero. That makes the
command useful in CI:

```bash
git mesh stale --format=github-actions
```

Machine-readable formats:

```bash
git mesh stale --format=porcelain
git mesh stale --format=json
git mesh stale --format=junit
git mesh stale --format=github-actions
```

Scope a CI run to ranges anchored on the current branch:

```bash
base="$(git merge-base origin/main HEAD)"
git mesh stale --since "$base" --format=github-actions
```

Use `--no-exit-code` when you want reporting without gating:

```bash
git mesh stale --no-exit-code --format=json > mesh-stale.json
```

## Team workflow

### Prefer small, named relationships

Mesh names are like branch names: short, stable, and meaningful. Good
names describe the relationship:

```text
billing-charge-contract
auth-token-verification
profile-schema-docs
rate-limit-config-tests
```

Avoid:

```text
misc
john-work
temp
frontend
```

One mesh should describe one relationship. If the ranges split into two
different reasons to change together, create two meshes.

### Write useful mesh messages

A good message answers:

- What relationship does this mesh represent?
- Why should a future reviewer care?
- Who owns or understands it?
- What should happen when it becomes stale?

Example:

```text
Checkout request matches charge API contract

The checkout UI builds the same request shape that api/charge.ts
validates. Review this mesh when userId, amount, idempotencyKey, or the
response status shape changes.

Owner: team-billing
```

This mirrors strong commit-message practice from Git and Mercurial
guides: future readers need intent, not just a restatement of the diff.

### Keep branches short-lived

Modern Git guidance generally favors short-lived branches merged through
review and CI. Long-lived branches make mesh state drift in private,
then produce noisy stale reports later. Prefer:

```text
feature branch -> source commit + mesh commit -> PR -> CI stale check -> merge
```

For release trains or regulated workflows, use release branches where
the process needs them, but keep mesh updates close to the code changes
that justify them.

### Review mesh changes in PRs

When a PR changes code covered by a mesh, reviewers should ask:

- Did `git mesh stale --since <merge-base>` run?
- Does the mesh message still describe the relationship?
- Are changed ranges re-anchored after intentional drift?
- Did a source change require adding a new range to an existing mesh?
- Did a removed feature require deleting or reverting a mesh?

Mesh commits are custom refs, so web hosts may not display them like
normal branches. Use CLI output in PR descriptions or CI annotations
when the hosting UI does not show custom refs.

### Sync explicitly

Push and fetch mesh data alongside code review:

```bash
git mesh push
git mesh fetch
```

`git-mesh` lazily configures fetch and push refspecs for
`refs/ranges/*` and `refs/meshes/*` on first use. The default remote is
`origin`, unless `mesh.defaultRemote` is set:

```bash
git config mesh.defaultRemote upstream
```

Use Git plumbing to inspect remote mesh refs when needed:

```bash
git ls-remote origin 'refs/meshes/*'
git ls-remote origin 'refs/ranges/*'
```

## CI patterns

### Pull request gate

```bash
git mesh fetch origin
base="$(git merge-base origin/main HEAD)"
git mesh stale --since "$base" --format=github-actions
```

This reports ranges anchored on the branch and emits annotations for
GitHub Actions.

### Full repository audit

```bash
git mesh fetch origin
git mesh stale --format=junit
```

Use this as a scheduled job if the repository has many relationships
that can drift without a nearby PR.

### Advisory report

```bash
git mesh stale --no-exit-code --format=json > mesh-report.json
```

Use this for dashboards or migration work where stale meshes are known
and should be counted rather than blocked.

### Setup audit

```bash
git mesh doctor
```

Run this in developer setup checks or as a lightweight CI step. It is
for repository health, not semantic drift.

## Troubleshooting

### `git mesh commit` says nothing is staged

Run:

```bash
git mesh status <name>
```

You may have committed source code without staging mesh operations, or
you may have cleared staging with `git mesh restore`.

### First commit requires a message

A new mesh has no parent message to inherit. Set one:

```bash
git mesh message <name> -m "Explain the relationship"
git mesh commit <name>
```

### A staged range has working-tree drift

The file changed after `git mesh add`. Re-stage the range:

```bash
git mesh restore <name>
git mesh add <name> path/to/file#L10-L20
git mesh message <name> -m "..."
```

If only one staged range drifted, you can remove and re-add the affected
operation in your normal workflow. The conservative full restore is
often faster and clearer.

### Duplicate range location

One mesh cannot contain two active ranges with the same
`(path, start, end)`. To re-anchor, remove the old range first:

```bash
git mesh rm <name> file.ts#L10-L20
git mesh add <name> file.ts#L10-L20
git mesh commit <name>
```

Overlapping ranges are allowed if their exact start/end pairs differ.

### Missing remote mesh data

Fetch mesh refs:

```bash
git mesh fetch
```

If the remote lacks refspecs, `git mesh fetch` or `git mesh push` will
bootstrap them when the remote is configured.

### Orphaned range

An `ORPHANED` range means the anchor cannot be materialized locally.
Common causes are missing fetches, force-pushes, aggressive garbage
collection, or partial repository state.

Try:

```bash
git fetch --all
git mesh fetch
git mesh stale <name>
```

If the anchor is truly gone, remove and re-anchor the range at a commit
that exists.

### `git log --all` shows mesh commits

Mesh commits live under custom refs, so all-ref traversals can see them.
Use scoped history aliases when you want normal branch/tag history:

```bash
git config alias.hist 'log --graph --branches --remotes --tags'
git log --all --exclude=refs/meshes/*
git config log.excludeDecoration refs/meshes/*
```

Range refs point at blobs, so log traversal does not walk them as commit
history.

## Internals worth knowing

You do not need the internals for daily use, but they make the tool less
mysterious.

### Storage

Range records are immutable blobs under `refs/ranges/v1/<id>`. Mesh
records are commits under `refs/meshes/v1/<name>`. A mesh commit's tree
contains:

```text
config
ranges
```

The commit message is the mesh message.

This is why mesh history works with normal Git concepts. The mesh ref is
the mutable name. The commit chain is the audit trail. The range blobs
are stable anchors.

### Staging

Staged mesh operations live under `.git/mesh/staging/`:

```text
<name>       pending add/remove/config operations
<name>.msg   staged message
<name>.<N>   sidecar bytes for staged add N
```

The sidecar is what lets the tool detect that the working tree changed
after you staged a range.

### File index

`.git/mesh/file-index` is a derived local lookup table. It lets
`git mesh ls <path>` answer "which meshes touch this file?" without
walking every mesh and range each time. It is regenerated when missing
or corrupt.

### Resolver

The resolver walks from the anchor commit to `HEAD`, follows file
movement/copy signals according to the mesh config, compares anchored
bytes with current bytes, and returns `FRESH`, `MOVED`, `CHANGED`, or
`ORPHANED`.

No status is stored. Staleness is always computed from the current local
repository state.

### Atomicity

Mesh commits validate staged operations before writing the mesh ref.
The implementation uses compare-and-swap style ref updates and retries
when another writer advances the same mesh ref concurrently. That gives
mesh edits the same shape of safety users expect from branch updates.

## Command reference

Reading:

```bash
git mesh
git mesh ls [<path>|<path>#L<start>-L<end>]
git mesh <name>
git mesh <name> --oneline
git mesh <name> --format=<fmt>
git mesh <name> --no-abbrev
git mesh <name> --at <commit-ish>
git mesh <name> --log [--limit <n>]
git mesh stale [<name>] [--format=human|porcelain|json|junit|github-actions]
git mesh stale [<name>] [--oneline|--stat|--patch] [--since <commit-ish>]
```

Staging and committing:

```bash
git mesh add <name> <range>... [--at <commit-ish>]
git mesh rm <name> <range>...
git mesh message <name> [-m <msg>|-F <file>|--edit]
git mesh commit [<name>]
git mesh status <name>
git mesh status --check
```

Configuration:

```bash
git mesh config <name>
git mesh config <name> <key>
git mesh config <name> <key> <value>
git mesh config <name> --unset <key>
```

Structural operations:

```bash
git mesh restore <name>
git mesh revert <name> <commit-ish>
git mesh delete <name>
git mesh mv <old> <new>
```

Sync and maintenance:

```bash
git mesh fetch [<remote>]
git mesh push [<remote>]
git mesh doctor
```

Reserved mesh names are command names. Do not name a mesh `add`, `rm`,
`commit`, `message`, `restore`, `revert`, `delete`, `mv`, `stale`,
`fetch`, `push`, `doctor`, `log`, `config`, `status`, `ls`, or `help`.

## Best-practice checklist

- Mesh the smallest range that carries the contract.
- Name the relationship, not the implementation detail.
- Write messages with intent, owner, and review guidance.
- Keep mesh changes in the same PR as source changes.
- Run `git mesh status <name>` before committing staged mesh work.
- Install the pre-commit and post-commit hooks or equivalent automation.
- Run `git mesh stale --since <merge-base>` in PR CI.
- Run full `git mesh stale` periodically on important repositories.
- Fetch before reviewing shared mesh state.
- Re-anchor intentionally; do not ignore `MOVED` or `CHANGED` findings.
- Use `ignore-whitespace` only when whitespace should not matter.
- Prefer many focused meshes over one broad mesh.
- Prefer `git mesh revert` when restoring prior truth.
- Use `git mesh doctor` when local behavior seems strange.

## Further reading

These sources shaped the practices in this handbook:

- [Git data model](https://git-scm.com/docs/gitdatamodel.html), for
  objects, references, reachability, the index, and custom `refs/`
  namespaces.
- [Pro Git](https://git-scm.com/book/en/v2.html), for Git branching,
  distributed workflows, internals, and everyday command habits.
- [GitHub Flow](https://docs.github.com/get-started/quickstart/github-flow),
  for lightweight branch-based collaboration and pull-request review.
- [GitHub pull request management](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/getting-started/managing-and-standardizing-pull-requests),
  for protected branches, required checks, and standardized review.
- [Atlassian feature branch workflow](https://www.atlassian.com/continuous-delivery/continuous-delivery-workflows-with-feature-branching-and-gitflow),
  for feature branches, pull requests, and review-centered collaboration.
- [Trunk-Based Development: short-lived feature branches](https://trunkbaseddevelopment.com/short-lived-feature-branches/),
  for keeping branches short and integration frequent.
- [Trunk-Based Development: continuous integration](https://trunkbaseddevelopment.com/continuous-integration/),
  for the relationship between integration discipline and automation.
- [Mercurial: The Definitive Guide](https://book.mercurial-scm.org/),
  for distributed-version-control principles, commit-message discipline,
  and the value of simple mental models.
- [Version Control with Subversion](https://svnbook.red-bean.com/en/1.7/svn-book.pdf),
  for branching, tagging, merging, and version-control practices that
  apply beyond one tool.

## Code and design references

- [git-mesh design](git-mesh.md)
- [CLI command surface](../packages/git-mesh/src/cli/mod.rs#L39-L318)
- [data model types](../packages/git-mesh/src/types.rs#L18-L113)
- [staging area implementation](../packages/git-mesh/src/staging.rs#L1-L404)
- [mesh commit pipeline](../packages/git-mesh/src/mesh/commit.rs#L19-L287)
- [stale resolver](../packages/git-mesh/src/stale.rs#L13-L171)
- [file index](../packages/git-mesh/src/file_index.rs#L1-L131)
- [sync refspecs](../packages/git-mesh/src/sync.rs#L7-L56)
- [doctor checks](../packages/git-mesh/src/cli/structural.rs#L63-L438)
