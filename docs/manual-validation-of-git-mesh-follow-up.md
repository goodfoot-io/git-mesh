# Manual validation follow-up — `git-mesh`

Follow-up pass on 2026-04-24 against `git-mesh 0.1.0` (binary at
`/home/node/.local/bin/git-mesh`). Re-verifying the critical bugs, minor
quality issues, and handbook doc bugs flagged in
`docs/manual-validation-of-git-mesh.md`. Scratch repos under
`/tmp/gm-followup/*`.

## Critical bugs

### 1. LFS line-range commit — **FIXED (with a new stale-layer bug)**

`git mesh commit` on a staged `data.tsv#L1-L10` (file tracked by
`filter=lfs`) now succeeds:

```
$ git mesh add pn data.tsv#L1-L10
$ git mesh why pn -m x
$ git mesh commit pn
updated refs/meshes/v1/pn
$ echo $?
0
```

Bounds are now checked against the sidecar's filtered line count (per
commit `9e034ac`), not the 3-line pointer blob.

🔴 **NEW BUG found while verifying**: immediately after a successful
`mesh commit pn` on an LFS line-range, with **no subsequent edits**,
`git mesh stale pn` reports:

```
# porcelain v1
CHANGED	W	pn	data.tsv	1	10	-
exit=1
```

JSON output shows `current.extent = L1-L57` versus `anchored.extent =
L1-L10`. The resolver's worktree-layer read of the LFS-filtered content
computes a different line count (57) than the sidecar captured at `add`
time (50). Either the LFS filter-process reader is emitting extra
newlines on re-read, or the line counter is running over raw bytes that
include `\r\n` that the sidecar normalized away. Reproduces in a
minimal repo (`/tmp/gm-followup/lfsrepo2`). Blocks the handbook's LFS
worked example at the `stale` step even though commit now succeeds.

### 2. Whole-file staged re-anchor ack — **FIXED**

Binary PNG re-anchor now produces `(ack)` and exits 0 (commit `d9ba0b1`
replaces the corrupting `String::from_utf8_lossy` + `str::replace`
normalization path with an attribute-gated byte-level walker).

```
$ git mesh stale web
  W CHANGED hero.png#L0-L0  (ack)
Pending mesh ops:
  ADD    hero.png whole (…)
exit=0
```

The spurious `(drift: sidecar mismatch)` is gone.

### 3. Duplicate staged `add` — **FIXED (last-write-wins)**

```
$ git mesh add m f.txt#L1-L10   # exit 0
$ git mesh add m f.txt#L1-L10   # exit 0 — previously errored
$ ls .git/mesh/staging/
m  m.1  m.1.meta
```

Staging slots collapse to a single sidecar; later `add` wins. Matches
the handbook's contract.

### 4. Sidecar tamper detection — **FIXED** (all three surfaces)

Integrity check via SHA-256 in `SidecarMeta.content_sha256` (commit
`d9ba0b1`) catches tamper at all three surfaces:

- **stale**: `ADD    f.txt L11-L20 ()  (drift: sidecar tampered)`, exit 1.
- **commit**: `error: sidecar tampered for mesh 'm' slot 1`, exit 2.
- **doctor**: `ERROR SidecarTampered: sidecar for mesh 'm' slot 1
  ('f.txt') failed integrity check — 'git mesh restore m' and re-stage
  'f.txt'`, exit 1.

Fail-closed on missing/empty hash as advertised.

### 5. `--since` filter — **FIXED**

With mesh `on-main` anchored at the seed commit and `on-feat` anchored
one commit later:

| `--since`          | Expected      | Actual                    |
|--------------------|---------------|---------------------------|
| (absent)           | both          | ✅ both                    |
| seed commit        | both (at-or-after seed) | ✅ both         |
| seed~-successor (feat's parent) | on-feat only | ✅ on-feat only |
| `HEAD`             | none          | ✅ none (exit 0)           |

Filtered ranges get a stderr annotation (`filtered 1 ranges anchored
before <sha>`), which is helpful for CI debugging. Handbook's PR-gate
recipe is now workable.

## Minor quality issues

| Item                               | Status |
|------------------------------------|--------|
| SIGPIPE panic on `\| head`         | ✅ **FIXED** — `git mesh m --no-abbrev \| head -2` exits cleanly. |
| Attribute-only binary detection    | ✅ **FIXED** — a NUL-bearing file without `.gitattributes` is now rejected: `error: line-range pin rejected on binary path`. |
| Duplicate refspec on repeat push   | ✅ **FIXED** — three successive `git mesh push` calls leave a single `+refs/ranges/*:refs/ranges/*` and a single `+refs/meshes/*:refs/meshes/*`. |
| `--at` order-sensitivity           | ✅ **FIXED** — both `git mesh add m <range> --at HEAD` and `git mesh add m --at HEAD <range>` succeed with identical results. |
| Reflog coverage (`core.logAllRefUpdates=always`) | Not manually reproduced this pass, but commit `4cb94f4` adds lazy-configuration + doctor `INFO` finding — trust the test coverage in `slice_5_6_integration.rs`. |

## Handbook doc bugs — **ALL 6 STILL PRESENT**

The six doc bugs raised in the original validation have not been
addressed in `docs/git-mesh-the-missing-handbook.md`. Each is still at
the same line number:

1. **L112–114 + L123**: hook installation snippet still says
   `git mesh pre-commit`. The subcommand is `git mesh pre-commit-check`
   (doctor recommends the correct one).
2. **L506 + L623 + L633**: whole-file render token is documented as
   `(whole)`, but the tool renders `#L0-L0` (human) and `0 0 -`
   (porcelain).
3. **L586–592**: "The range surfaces twice — once per drifting layer"
   and the paired two-row example still contradict the implementation's
   shallowest-layer-only behavior.
4. **L965**: `git log --all --exclude=refs/meshes/*` still recommended;
   `--exclude` does not affect `--all` in the way implied.
5. **L1003**: staging layout still omits the `.meta` sidecar files.
6. **Copy-detection mode efficacy (§Changing resolver settings)**: no
   textual guidance added about the minimum similarity threshold or
   fixture shape required to observe `same-commit` vs
   `any-file-in-commit` difference. (This is the softer one of the six;
   the others are concrete corrections.)

See "Doc bug discussion" below.

## New finding introduced during verification

🔴 **LFS line-range stale false-positive** (detailed under Critical
bug 1 above): the post-commit freshness check over an LFS line-range
reports a worktree-layer `CHANGED` with a line-count mismatch
(sidecar 50, worktree read 57) immediately after commit with no
edits. This is a separate defect from the original "commit rejects
line-range" bug and blocks the LFS worked example in the handbook.
Likely lives in the LFS filter-process reader path in
`resolver/engine/…` or in the sidecar stamp normalization. Worth a
dedicated slice.

## Summary

- **Critical bugs 1–5**: 4 fully fixed (2, 3, 4, 5); 1 partially fixed
  (LFS commit works, stale regresses).
- **Minor quality issues**: 4 of 4 manually verified as fixed; 1
  (reflog coverage) trusted to integration tests.
- **Doc bugs**: 6 of 6 still in the handbook.
- **New bug**: LFS line-range stale emits a false-positive `CHANGED W`
  with line-count drift from 10 → 57 on the first post-commit stale
  run. Needs its own slice.
