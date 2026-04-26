# Whole-file pins and LFS-tracked paths

## Whole-file pins

Drop the `#L...` suffix at `git mesh add` to pin an entire file. Output shows `(whole)` in place of the line range. `CHANGED` means "blob OID differs" rather than "lines drifted." There is no slice diff and no per-line culprit — the signal is "the bytes of this file are not what they were when you pinned it."

```bash
git mesh add brand-refresh marketing/hero.png
git mesh add api-contract-v2 vendor/openapi-spec
git mesh add diagram-refs docs/architecture.drawio
git mesh add charge-msa legal/msa.md
git mesh commit brand-refresh
```

Use whole-file when the file is **consumed as a unit by name or identity** — its bytes-as-a-whole are the thing the partner depends on:
- Images and binary assets pinned next to the copy that describes them
- Design diagrams (PNG/SVG) alongside the code they document
- Prose documents whose identity is the contract: a license, a one-page ADR, a published RFC, a signed MSA, an SOC2 control narrative
- Submodule roots (gitlink paths) — bumps surface for review of partner code without opening the submodule
- Symlinks — compared by target string
- Generated or minified assets
- Binary test fixtures next to the test that feeds them

Whole-file is also the **recommended default for prose meshes**: line-ranged prose drifts noisily under editorial churn (heading renumbers, reflow, sentence rewrites that preserve meaning). See `./responding-to-drift.md` for the prose-drift workflow.

## Rejections at `git mesh add`

- **Line-range pin on a binary path, symlink, or a path inside a submodule** — The error points at the whole-file form.
- **Whole-file pin on a path *inside* a submodule** — Only the submodule root (the gitlink path) itself accepts a whole-file pin.

## LFS-tracked paths

Ranges on `filter=lfs` paths behave like non-LFS ranges as long as the content is locally cached. `git mesh add` reads real bytes through the LFS filter and takes its anchored slice from them. Line-range and whole-file forms both work.

```bash
git lfs fetch
git mesh add perf-notes benchmarks/results.tsv#L1-L200
```

If content is not cached:
- `git mesh add` fails with a message pointing at `git lfs fetch`.
- A pinned range at stale time surfaces as `CONTENT_UNAVAILABLE(LfsNotFetched)`. See `./content-unavailable.md`.

The fast path for LFS whole-file pins is pointer-OID equality.

## Submodule roots

A whole-file pin on the submodule path compares gitlink SHAs without opening the submodule. Submodule bumps surface as `CHANGED` with no slice diff — exactly the signal needed for "review the partner code before merging."

A line-range pin inside a submodule is rejected at `git mesh add`. Legacy ranges pointing inside a submodule surface as the `SUBMODULE` terminal status; see `./terminal-statuses.md` to migrate them.

## Re-anchoring whole-file pins

Same as line ranges: a second `git mesh add` over the same path is a re-anchor (last-write-wins), and the staged add shows `(ack)` next time stale runs.

```bash
git mesh add brand-refresh marketing/hero.png
git mesh commit brand-refresh
```
