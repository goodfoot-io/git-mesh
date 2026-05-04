# Command reference

## Anchor grammar

- **Line-range anchor**: `<path>#L<start>-L<end>` — 1-based, inclusive.
- **Whole-file anchor**: `<path>` alone — no `#L…` suffix. See `./whole-file-and-lfs.md`.

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

Resolution rule: bare words (no `/`, no `#L`) try mesh-name first; everything else is a path. A target that resolves to no meshes is fine on its own — `list` exits 0 with an empty result; `stale` exits 0 silently. The command only errors when a target names a referent that doesn't exist (missing file, missing mesh name, unmatched literal glob).

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

## Structural

```bash
git mesh restore <name>
git mesh revert  <name> <commit-ish>
git mesh delete  <name>
git mesh move      <old> <new>
```

## Sync and maintenance

```bash
git mesh fetch  [<remote>]
git mesh push   [<remote>]
git mesh doctor
```

## Reserved mesh names

Do not name a mesh after a command: `add`, `remove`, `commit`, `why`, `restore`, `revert`, `delete`, `move`, `stale`, `fetch`, `push`, `doctor`, `log`, `config`, `list`, `help`.
