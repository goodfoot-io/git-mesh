# Command reference

## Range grammar

- **Line range**: `<path>#L<start>-L<end>` — 1-based, inclusive.
- **Whole file**: `<path>` alone — no `#L…` suffix. See `./whole-file-and-lfs.md`.

## Reading

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
git mesh stale [<name>] [--no-worktree] [--no-index] [--no-staged-mesh]
git mesh stale [<name>] [--ignore-unavailable] [--no-exit-code]
```

## Staging and committing

```bash
git mesh add <name> <range>... [--at <commit-ish>]
git mesh rm  <name> <range>...
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
git mesh mv      <old> <new>
```

## Sync and maintenance

```bash
git mesh fetch  [<remote>]
git mesh push   [<remote>]
git mesh doctor
```

## Reserved mesh names

Do not name a mesh after a command: `add`, `rm`, `commit`, `why`, `restore`, `revert`, `delete`, `mv`, `stale`, `fetch`, `push`, `doctor`, `log`, `config`, `ls`, `help`.
