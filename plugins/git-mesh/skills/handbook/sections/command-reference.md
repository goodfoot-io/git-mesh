# Command reference

## Anchor grammar

- **Line-range anchor**: `<path>#L<start>-L<end>` — 1-based, inclusive.
- **Whole-file anchor**: `<path>` alone — no `#L…` suffix. See `./whole-file-and-lfs.md`.

`#` is a shell comment character; quote anchors when scripting (`'src/auth.ts#L10-L20'`).

## Reading

```bash
git mesh
git mesh list [<target>...]
git mesh <name>
git mesh <name> --oneline
git mesh <name> --format=<fmt>
git mesh <name> --no-abbrev
git mesh <name> --at <commit-ish>
git mesh <name> --log [--limit <n>]
git mesh stale [<target>...] [--format=human|porcelain|json|junit|github-actions]
git mesh stale [<target>...] [--oneline|--stat|--patch] [--since <commit-ish>]
git mesh stale [<target>...] [--no-worktree] [--no-index] [--no-staged-mesh]
git mesh stale [<target>...] [--ignore-unavailable] [--no-exit-code]
```

Each `<target>` is one of: a mesh name, a file path, or — for `list` only — a line-range address `<path>#L<start>-L<end>`. Globs are expanded by the shell. Multiple targets are unioned and deduplicated.

Resolution rule: each argument is tried as a mesh name first when it has the mesh-name shape (kebab-case segments, optionally separated by `/` — e.g. `auth-token`, `billing/payments/checkout-request-flow`). It falls through to path-index lookup when no mesh matches, then to a worktree existence check. A `#L<start>-L<end>` suffix marks a range address. A target that resolves to no meshes is fine on its own — `list` exits 0 with an empty result; `stale` exits 0 silently. The command only errors when a target names a referent that doesn't exist (missing file, missing mesh name, unmatched literal glob).

Silent exit-0 from `git mesh stale` (and `list`) means the queried scope is clean. See `./reading-stale-output.md` § "No-news-is-good-news".

## Staging and committing

```bash
git mesh add <name> <anchor>... [--at <commit-ish>]
git mesh remove  <name> <anchor>...
git mesh why <name>                                 # print current why
git mesh why <name> [--at <commit-ish>]             # print historical why
git mesh why <name> [-m <text>|-F <file>|--edit]    # stage a new why
git mesh commit [<name>]
```

## Configuration

```bash
git mesh config <name>
git mesh config <name> <key>
git mesh config <name> <key> <value>
git mesh config <name> --unset <key>
```

Keys: `copy-detection` (`off` | `same-commit` | `any-file-in-commit` | `any-file-in-repo`), `ignore-whitespace` (`true` | `false`).

Copy-detection values:
- **`off`** — strict rename-only or no copy tracking.
- **`same-commit`** — default; good balance for ordinary refactors.
- **`any-file-in-commit`** — code may be copied from another file touched in the same commit.
- **`any-file-in-repo`** — last resort; broad and can be expensive.

`ignore-whitespace true` is appropriate for formatting churn; it is wrong if whitespace is semantically meaningful.

Config is mesh state: staged, committed, and shared by every consumer of the mesh.

## Structural

```bash
git mesh restore <name>
git mesh revert  <name> <commit-ish>
git mesh delete  <name>            # refuses while staging is non-empty
git mesh move      <old> <new>
```

`git mesh delete` refuses while staged ops remain for `<name>`; run `git mesh restore <name>` first.

## Sync and maintenance

```bash
git mesh fetch  [<remote>]
git mesh push   [<remote>]
git mesh doctor
```

## Reserved mesh names

Do not name a mesh after a command: `add`, `remove`, `commit`, `why`, `restore`, `revert`, `delete`, `move`, `stale`, `fetch`, `push`, `doctor`, `log`, `config`, `list`, `help`.
