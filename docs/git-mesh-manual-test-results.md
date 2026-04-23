# git-mesh manual test results

One-time sanity check of `git mesh` behavior against `docs/git-mesh-the-missing-handbook.md`. All tests run in throwaway repos under `/tmp`. `git-mesh 0.1.0`.

## Summary

| # | Area | Result | Notes |
|---|---|---|---|
| 0 | Setup + `doctor` on fresh repo | PASS* | Exits 1 with only INFO/WARN findings (see 0-1). |
| 1 | Smoke: add → message → commit → read | PASS | Refs, tree (`config`+`ranges`), log all correct. |
| 2 | Range syntax & validation | PASS* | Duplicate ranges are rejected at commit, not at `add` (see 2-1). |
| 3 | Staging drift detection | PASS | `status` and `status --check` both show diff; check exits 1. |
| 4 | Stale states | PARTIAL | FRESH/MOVED/CHANGED work; `ORPHANED` never produced — see 4-1. |
| 5 | Exit codes & machine formats | PASS | json/junit/github-actions/porcelain all valid; `--no-exit-code` works. |
| 6 | Re-anchor workflow | PASS* | `rm` of a range not present does not error (see 6-1). |
| 7 | Structural ops (mv / revert / delete) | PASS | All ref state as expected; reverts leave dangling commits (expected). |
| 8 | Reserved names | PASS | All 17 names rejected with exit 2. |
| 9 | `ls` and file-index regeneration | PASS | Overlap semantics correct; `doctor` regenerates index. |
| 10 | Sync push/fetch | PASS | Bare remote round-trip works; refspecs auto-configured. |
| 11 | Hooks | FAIL | Handbook's post-commit (`git mesh commit` with no name) errors — see 11-1. |
| 12 | Concurrency | PASS | Different-mesh parallel commits both land; same-mesh serializes safely; no fsck errors. |

Legend: PASS = matches handbook; PASS\* = works but with a caveat documented below; PARTIAL = one scenario couldn't be reproduced; FAIL = behavior differs from handbook.

## Findings

### 0-1. `git mesh doctor` exits 1 with only informational findings

On a fresh repo, `doctor` prints `INFO` findings for missing pre-commit/post-commit hooks and a `WARN` for the missing file-index, then exits **1**. If `doctor` is used in CI setup checks, this will trip. Either the exit code should be 0 when only INFO/WARN are present, or the handbook should call this out.

### 2-1. Duplicate ranges stage successfully, fail at commit

`git mesh add dup file.txt#L1-L2 file.txt#L1-L2` exits 0 and writes both operations to staging. Only `git mesh commit dup` fails with `error: duplicate range location in mesh: file.txt:1-2`. The handbook phrasing ("The tool rejects two active ranges...") implies earlier rejection. Minor.

### 4-1. `ORPHANED` status not produced by ref deletion

I deleted all `refs/ranges/v1/*` to simulate a missing anchor. `git mesh stale m1` errored:

```
error: range not found: 52a4bf91-...
exit=2
```

rather than reporting the documented `ORPHANED` status through the normal stale output. The `ORPHANED` path may only be reachable via a different failure mode (force-pushed anchor commit, gc'd blob in a partial clone). Worth verifying — the handbook, the status table, and the JSON schema all advertise `ORPHANED` as a normal stale status, so it should be reachable through at least one reproducible scenario and the common "range ref missing" case should either produce it or be documented as a different failure.

### 6-1. `git mesh rm <name> <range-not-present>` does not error

Removing a range that is not in the mesh silently succeeds and stages an op. This makes typos in re-anchor workflows harder to catch. A warning or non-zero exit would be safer per the repo's fail-closed guidance.

### 11-1. Handbook post-commit hook is wrong

The handbook suggests:

```sh
# .git/hooks/post-commit
#!/bin/sh
git mesh commit
```

Running this produces:

```
error: `git mesh commit <name>` requires a name
exit=2
```

The CLI requires a mesh name. Either the CLI should default to committing all staged meshes when no name is given (matches the documented workflow and the hook's purpose), or the handbook must be corrected. The hook purpose ("stage mesh updates before a source commit and anchor them to the commit that just landed") implies the CLI default should exist — otherwise you need a hook that knows every staged mesh name.

The pre-commit hook (`git mesh status --check`) works as documented and correctly blocked a commit with staging drift.

## Evidence log (abbreviated)

Fixture: `mktemp -d` repo, single file `file.txt` with 6–8 lines, standard seed commit.

- Task 1 smoke: `refs/meshes/v1/m1` commit has tree `config` + `ranges`, range blob under `refs/ranges/v1/<uuid>`.
- Task 4 stale: FRESH exit 0; prepend line → `MOVED file.txt#L2-L4 → file.txt#L3-L5`; in-place edit → `CHANGED` with culprit commit; delete range ref → hard error.
- Task 5 formats: `jq .` accepts JSON; `python -c 'ET.fromstring'` accepts JUnit; github-actions prints `::error file=…,line=…::…`.
- Task 10 sync: `git ls-remote ../remote.git 'refs/meshes/*' 'refs/ranges/*'` lists all refs after `git mesh push`; wiping local refs and running `git mesh fetch` restores them.
- Task 12 concurrency: two parallel `git mesh commit <name>` against different meshes both succeed; against same mesh, one succeeds and the other returns a `duplicate range location` error. `git fsck` reports only expected dangling commits (from earlier revert), no errors.

## Recommended follow-ups

1. Fix or document handbook post-commit hook (finding 11-1). Highest-impact; every team that copies the handbook will hit it.
2. Make `doctor` exit 0 for INFO/WARN-only findings, or document the exit-code contract (finding 0-1).
3. Reproduce `ORPHANED` through a supported path, or align the docs (finding 4-1).
4. Consider erroring on `rm` of a range not present in the mesh (finding 6-1).
5. Consider rejecting duplicate ranges at `add` time (finding 2-1).
