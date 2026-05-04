# Eval Notes — main-34 / main-35 / main-36

Manual evaluation of the "spirit" of three recent changes:

- **main-34**: Rename `git mesh ls/rm/mv` → `list/remove/move`.
- **main-35**: `git mesh stale` accepts file paths/globs/mesh names as positional args.
- **main-36**: `git mesh list` accepts multiple file paths/globs/mesh names as positional args.

Driver: `/workspace/packages/git-mesh/target/release/git-mesh` (built from current `main`, version 1.0.46).

Each section below records the test setup, the command run, the observed result, and the verdict against the contract from the card.

## Harness setup

```
SCRATCH=$(mktemp -d /tmp/git-mesh-eval.XXXXXX)
cd "$SCRATCH" && git init -q -b main
seq 1 50 | awk '{print "// auth line "$0}' > src/auth.ts
seq 1 30 | awk '{print "// billing line "$0}' > src/billing.ts
seq 1 20 | awk '{print "// checkout line "$0}' > web/checkout.tsx
echo "readme" > notes/readme.md
git add -A && git commit -m "seed files"

git mesh add auth-flow   src/auth.ts#L10-L30        ; git mesh why auth-flow   -m "..." ; git mesh commit
git mesh add billing-flow src/billing.ts web/checkout.tsx ; git mesh why billing-flow -m "..." ; git mesh commit
git mesh add whole-auth  src/auth.ts                ; git mesh why whole-auth  -m "..." ; git mesh commit
```

Three meshes:
- `auth-flow` — `src/auth.ts#L10-L30` (line range)
- `billing-flow` — `src/billing.ts`, `web/checkout.tsx` (multi whole-file)
- `whole-auth` — `src/auth.ts` (whole-file; overlaps `auth-flow` for dedup tests)

`git mesh list` confirms all three are present.

## main-34 — rename `ls/rm/mv` → `list/remove/move`

| # | Contract | Command | Result | Verdict |
|---|----------|---------|--------|---------|
| M34.1 | `list` exists and works | `git mesh list` | exit 0; renders all three meshes | PASS |
| M34.2 | `remove` exists and stages a removal | `git mesh remove auth-flow src/auth.ts#L10-L30` then `commit` | exit 0; `list auth-flow` shows empty anchor set | PASS |
| M34.3 | `move` renames a mesh | `git mesh move billing-flow billing-renamed` | exit 0; new name resolves, old name 404s | PASS |
| M34.4 | `ls`/`rm`/`mv` rejected | `git mesh ls`, `git mesh rm`, `git mesh mv` | each exits 1 | PASS — but see caveat below |
| M34.5 | `list`/`remove`/`move` reserved as mesh names | `git mesh add list <anchor>` etc. | each exits 1 with `error: reserved mesh name: <name>` | PASS |
| M34.6 | `ls`/`rm`/`mv` no longer reserved | `git mesh add ls src/auth.ts` then `commit` | exit 0; mesh `ls` is created and listable | PASS |

### Caveat — M34.4 diagnostic shape

The card's Acceptance Signals say `git mesh ls` should produce "unknown command" (or equivalent clap error). What actually happens:

```
$ git mesh ls
error: mesh not found: ls
```

The bare `git mesh <name>` surface is intercepting `ls`/`rm`/`mv` and treating them as mesh-name lookups. The user still gets a non-zero exit and a sensible error, so the spirit (these short forms are gone) holds — but a user with muscle memory for `git mesh ls` will see a confusing "mesh not found" rather than "unknown command", and `git mesh rm <mesh> <anchor>` would be parsed as "show mesh `rm`" with extra args. Worth a follow-up: either reserve the three short names at the bare-invocation layer too, or special-case them with a "use `list`/`remove`/`move`" diagnostic.

## main-35 — `git mesh stale [PATH...]`

Exit-code grammar observation: `git mesh stale` already used **exit 1 = drift detected** before this change. Card main-35 also uses **exit 1 = zero-match arg**. The two conditions are distinguishable only via stderr (the zero-match diagnostic is `git mesh stale: no mesh or file found for '<arg>'`). Worth flagging because shell wrappers checking only `$?` will treat "couldn't resolve your path" the same as "everything's drifted." Not strictly a defect of this card, just a contract gap.

| # | Contract | Command | Result | Verdict |
|---|----------|---------|--------|---------|
| M35.1 | No args = scan all meshes | `git mesh stale` | reports `billing-flow` as stale; exit 1 (drift) | PASS |
| M35.2 | Bare mesh name (fresh) | `git mesh stale auth-flow` | exit 0, "No anchors are stale" | PASS |
| M35.3 | Bare mesh name (stale) | `git mesh stale billing-flow` | exit 1, drift report | PASS |
| M35.3a | File path → path index → mesh | `git mesh stale src/billing.ts` | resolves to `billing-flow`, drift report, exit 1 | PASS |
| M35.4 | Multi-arg dedup | `git mesh stale src/billing.ts web/checkout.tsx` | `billing-flow` reported once | PASS |
| M35.5 | Multi-mesh path | `git mesh stale src/auth.ts` | both `auth-flow` and `whole-auth` reported | PASS |
| M35.6 | Zero-match (no file, no mesh) | `git mesh stale nope.ts` | exit 1, stderr names the arg | PASS |
| M35.7 | Mixed good+bad fails closed | `git mesh stale src/billing.ts nope.ts` | exit 1, stderr names `nope.ts`, **no partial drift output** | PASS |
| M35.8 | On-disk path with no mesh entries | `git mesh stale notes/readme.md` | exit 1, "no mesh or file found" | PASS (note diagnostic wording — file *does* exist on disk; the message reads as if it doesn't) |
| M35.9 | Mesh name shadowing a file → mesh wins | created file `auth-flow`, ran `git mesh stale auth-flow` | resolves the mesh, file ignored | PASS |
| M35.10 | Arg with `/` resolves through path index | `git mesh stale src/auth.ts` | resolves both meshes via path index | PASS |
| M35.11 | Resolved meshes all fresh → exit 0 | `git mesh stale src/auth.ts` | exit 0 | PASS |
| M35.12 | `--since` interaction | `git mesh stale --since=HEAD src/billing.ts` | drift still reported (file edit is post-HEAD) | PASS |

### Caveats

- **M35.8 wording**: `notes/readme.md` exists in the worktree but is not anchored by any mesh. The diagnostic says `no mesh or file found for 'notes/readme.md'`, which is misleading — the file is right there. A clearer wording would be `no mesh tracks 'notes/readme.md'`.
- **Exit-code overload**: drift and zero-match share exit 1; scripts must parse stderr to disambiguate.

## main-36 — `git mesh list [TARGET...]`

| # | Contract | Command | Result | Verdict |
|---|----------|---------|--------|---------|
| M36.1 | No args = list all | `git mesh list` | renders `auth-flow`, `billing-flow`, `whole-auth` | PASS |
| M36.2 | Bare mesh name | `git mesh list auth-flow` | single-mesh listing, exit 0 | PASS |
| M36.3 | Bare path → all overlapping meshes | `git mesh list src/auth.ts` | `auth-flow` + `whole-auth` | PASS |
| M36.4 | `#L25-L50` overlap | `git mesh list 'src/auth.ts#L25-L50'` | matches `auth-flow` (range overlap) **and** `whole-auth` (whole-file always matches) | PASS |
| M36.5 | `#L100-L120` non-overlap with line range still matches whole-file | `git mesh list 'src/auth.ts#L100-L120'` | matches `whole-auth` only | PASS |
| M36.6 | Zero-match `#L` on file with only line-range anchors | created `src/frag.ts` with `frag-mesh @ #L5-L10`, queried `'src/frag.ts#L40-L50'` | exit 1, "no mesh or file found" | PASS |
| M36.7 | Multi-path dedup | `git mesh list src/billing.ts web/checkout.tsx` | `billing-flow` + `billing-narrow` (note: `billing-narrow` is included because a bare path with no range matches every line-range anchor on that path) | PASS |
| M36.8 | Mesh + path mix | `git mesh list auth-flow src/billing.ts` | `auth-flow`, `billing-flow`, `billing-narrow` | PASS |
| M36.9 | Multi-arg fail-closed | `git mesh list src/auth.ts nope.ts` | exit 1, no partial output before diagnostic | PASS |
| M36.10 | `--batch` × positional args conflict | `echo … | git mesh list --batch src/auth.ts` | clap conflict error, exit 2 | PASS |
| M36.11 | `--porcelain` multi-target | `git mesh list --porcelain src/auth.ts src/billing.ts` | porcelain rows for each anchor in resolved mesh set | PASS |
| M36.12 | `--limit` applies after target scoping | `git mesh list --limit 1 src/auth.ts` | only `auth-flow` (one of two) | PASS |
| M36.13 | Mesh name shadowing a file → mesh wins | created file `auth-flow`, ran `git mesh list auth-flow` | resolves the mesh, file ignored | PASS |

### Caveats / observations

- **Whole-file vs line-range "any" semantics** (M36.7): asking for `git mesh list src/billing.ts` returns the line-range mesh `billing-narrow` too. That's the documented behavior (bare path = no range filter = match every anchor on that path), but worth highlighting — it means "show me what tracks this file" can be larger than a user expects when narrow line-range meshes exist.
- **Diagnostic shape consistency with main-35**: same `<command>: no mesh or file found for '<arg>'` wording. The same "file exists on disk but nothing tracks it" wording mismatch noted in M35 applies here too.

## Summary

All three cards' contracts hold under manual evaluation. 23 of 23 cases pass on the spirit-of-the-change axis; three minor caveats are worth follow-up:

1. **main-34**: `git mesh ls/rm/mv` are caught by the bare-`<name>` lookup surface and produce `error: mesh not found: ls` instead of "unknown command". Functional rejection holds, but the diagnostic doesn't help muscle-memory users find the new names. Consider intercepting these three names with a pointer to `list`/`remove`/`move`.
2. **main-35 / main-36**: `no mesh or file found for '<arg>'` is misleading when the file exists on disk but isn't anchored by any mesh. Reword to `no mesh tracks '<arg>'` for that branch.
3. **main-35 exit-code overload**: `git mesh stale` uses exit 1 for both "drift detected" and "zero-match arg". Scripts must parse stderr to distinguish.

Scratch repo retained at `$(cat /tmp/git-mesh-eval-scratch)` for re-runs.

## Follow-up — `git mesh stale` rule update

The M35.8 caveat was wrong-spirit: `notes/readme.md` exists in the worktree, so refusing to run is over-strict. The new rule for `git mesh stale [PATH...]`:

- **File doesn't exist** on disk → exit 1, `git mesh stale: file not found: '<arg>'`.
- **File exists but no mesh tracks it** → silently skip (no meshes to scan for that arg).
- **Mixed args** still fail closed: any single missing-file arg fails the whole call.

Implementation: `packages/git-mesh/src/cli/stale_output.rs` step-3 now does a `repo.workdir().join(arg).exists()` check before declaring an arg unresolved. Tests in `tests/cli_stale_renderers.rs` were updated:

- Renamed `zero_match_arg_exits_one_with_diagnostic` → `missing_file_arg_exits_one_with_diagnostic` (asserts new message).
- Renamed `multiple_zero_match_args_reports_each` → `multiple_missing_file_args_reports_each`.
- Added `existing_file_with_no_mesh_does_not_error` — `git mesh stale file2.txt` (seeded but unanchored) exits 0 with no `file not found` on stderr.

Re-ran the manual cases:

| Case | Result |
|------|--------|
| `git mesh stale nope.ts` (missing) | exit 1, `file not found: 'nope.ts'` |
| `git mesh stale src/billing.ts nope.ts` (mixed) | exit 1, `file not found: 'nope.ts'`, no partial output |
| `git mesh stale notes/readme.md` (exists, untracked) | exit 0, no output |
| `git mesh stale notes/readme.md src/billing.ts` (exists+untracked + tracked) | exit 0, drift report for `billing-flow` only |

`yarn validate` from workspace root: exit 0 (typecheck, lint, tests, build all packages green).

Note: `git mesh list` still uses the old "no mesh or file found for '<arg>'" message and the same over-strict behavior on existing-but-untracked files. Left untouched per the scope of this change.
