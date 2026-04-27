# Techniques reference

`mine.mjs` runs 13 independent signals over the git history. Each is a noisy
predictor on its own; the aggregate ranking in §0 of the report is what matters.
This document explains what each technique measures, when it is trustworthy, and
how to read its specific output section.

## 1. File-level co-change (all commits)

The classic logical-coupling signal. For every commit touching ≥2 files, every
unordered pair of files gets weighted support `1 / log2(n+1)` where `n` is the
commit's file count. Weighting penalizes giant commits whose pairs are
mostly accidental.

**Confidence** is `max(P(B|A), P(A|B))` — if A almost always travels with B,
the pair is interesting even if B sometimes travels alone.

**Hidden-coupling score** (the rerank used in §1) multiplies support × confidence
× `(1 − authors∩ jaccard)`. The intuition: if every change to A and B comes
from the same author, the coupling lives in someone's head and is not
"hidden" — it is just personal context. The interesting case is high
support × confidence with low author overlap, because nobody currently owns
the contract.

**`[structural]` tag**: pairs where one file's basename literally appears in
the other file's contents (imports, manifest references, doc links). Such
coupling is real but already explicit, so these pairs are pushed to the
bottom of §1.

## 2. File-level co-change (bug-fix commits only)

The same calculation restricted to commits whose subject or body matches the
fix regex (default: `fix|bug|regression|hotfix|patch|revert|restore|tighten|
…loophole|closes #N`). Override with `--fix-regex=…` if your team uses
different vocabulary.

Bug-fix co-change is higher signal than general co-change because the pairs
were specifically forced together by a defect — they are the empirical
contract surface. If §2 is empty (`fix_commits=0` in the meta), the regex
does not match the codebase's commit conventions; supply your own.

## 3. Commit-message clustering

Pairs files by shared ticket key (`PROJ-123`) or conventional-commit scope
(`feat(scope): …`). Useful when teams use rigorous commit conventions; useless
otherwise (the section will simply be empty).

## 4. Cross-file range pairs (anchored)

Like §1 but at sub-file granularity. Each diff hunk is bucketed by its
enclosing function or heading (extracted from git's `@@` xfuncname output)
or, failing that, a 100-line bucket. Anchored bucketing survives line-number
drift, so a function that gets pushed down by an insertion above still
collapses with its own past hunks.

**When this section fires**: a specific function in A and a specific section in
B repeatedly co-change. Far more actionable than file-level coupling because
it tells you *where* in each file the contract lives.

## 5. Author divergence (inverse) and 5b. Author clusters (positive)

`authors∩ jaccard` is computed on every §1 pair. It feeds two outputs:

- **Inverse** — the hidden-coupling rerank in §1 multiplies by `(1 − jaccard)`
  so pairs nobody jointly owns rise to the top. That is the latent-contract
  case: the coupling exists and nobody currently holds it in their head.
- **Positive** — §5b lists pairs with **high** authors∩ jaccard
  (≥ `--author-jaccard-min`, default 0.7) **and different parent directories**.
  These are tribal subsystems whose conceptual unit cuts across the directory
  tree. The directory split prevents pairs that already live in the same
  module from showing up as "discoveries."

## 6. Apriori frequent itemsets and 6b. Association rules

Frequent k-itemsets computed via Apriori (downward closure: a k-itemset can
only be frequent if all its (k−1)-subsets are). Itemsets reveal "hub-and-
spoke" structures that pairs miss — e.g. one config file bridging two
implementation files.

The `--itemset-k` flag (default 3, max 5) controls the maximum itemset size.
For each frequent itemset of size ≥2, every nonempty proper-subset split is
turned into a **rule** `X → Y` and reported in §6b with:

- **support** — how often X∪Y co-occur
- **confidence** — P(Y | X)
- **lift** — `confidence / P(Y)`. >1 means positive association; ROSE-style
  change-prediction relevance kicks in around 1.5 (`--min-lift`, default 1.5).
- **leverage** — `P(X∪Y) − P(X)·P(Y)`, an absolute measure of departure from
  independence; useful for ordering rules whose lift is similar.

## 7. Directional lagged co-change

Co-change within a sliding window of `--window` commits (default 5), capped
at 7 days between commits. Direction matters: the result lists `earlier →
later`, meaning "every time `earlier` changes, `later` tends to change
within a few commits."

Lag is a strong "forces" hint. If A → B with no concurrent co-change, then
A's change *propagates* to B — often because B reacts to a contract A defines
(consumer adapting to producer).

## 8. Branch / merge topology

For every merge commit, lists the files touched on the merged-in branch
between the merge base and the tip. Captures the "what shipped together as
one feature" grouping that gets flattened by squash merges.

Useful for new-onboarding ("show me what files belong to one logical change")
and for noticing files that ship together but live in unrelated parts of the
tree.

## 9. Cross-language definition-anchored symbol co-change

For every multi-language commit, takes the set of identifiers *defined* on a
+/− line (matched via `class|interface|struct|enum|trait|type|fn|func|def|
function|const|let|var|impl <Name>` keywords across TS/JS, Rust, Python, Go,
Java/Kotlin, C/C++) and counts how often each symbol shows up across pairs of
languages.

A symbol that repeatedly shows up in commits crossing, say, Rust and TS is a
likely contract surface — an FFI boundary, a serialized type, a wire
protocol. The cross-language tag tells you *where* to look, not just that the
symbol is shared.

## 10. Coordinated rename / move chains

Multi-file renames within a single commit. Surfaces large refactors that
moved several files in lockstep — useful for spotting "this group of files
is one logical module" that was already recognized as such by a past author.

## 11. Churn correlation (Pearson + Spearman + lag)

Per-file weekly churn (lines added + removed) is computed for files
appearing in the §1 candidate pool, then run through three views and the
strongest is reported:

- **Pearson** — linear co-movement
- **Spearman** — rank-based, robust to outliers and non-linear-but-monotonic
  breathing (catches release-cycle pairs that vary in magnitude)
- **Lagged** — best |r| across lag ∈ [−4, +4] weeks; catches
  leader/follower pairs that breathe in phase but with a few-week shift
  (e.g. release ↔ docs)

Pairs with `|r| ≥ 0.6` and ≥6 overlapping weeks are reported. The output
shows which view won (`pearson` / `spearman`) and the lag in weeks.

**Restriction to candidates** is critical: full pairwise correlation is O(N²)
over all files and is the script's main cost driver if not pruned. The
script does the prune automatically.

## 9b. Cross-language SCREAMING_SNAKE / shared constants

§9 only counts symbols *defined* in the diff, missing the case where a
constant like `MAX_RETRY_COUNT` or a protocol field name appears as a bare
reference (consumer side) on one language and as a definition or reference
on the other. §9b relaxes the requirement to "mentioned on +/− lines on
both sides" but restricts to identifiers that look like protocol constants:
SCREAMING_SNAKE_CASE or PascalCase with len ≥ 6. Higher recall, more noise
than §9 — read the examples to filter.

## Boundary / sweep filters and aggregator weighting

Two filters are applied before any technique runs, to keep artificial
commits out of the coupling pool:

- **Lockfile-as-boundary**: commits touching a lockfile (yarn.lock, Cargo.lock,
  go.sum, …) or whose subject matches a dep-bump pattern (chore(deps), bump,
  upgrade, dependabot, renovate) are excluded entirely. Their file
  co-occurrences are dependency-bump artifacts, not semantic coupling.
- **Codemod sweeps**: commits with ≥8 files where per-file churn variance
  is near zero (coefficient of variation < 0.2) are excluded. Formatting
  passes, license-header rewrites, and regex codemods all match.

The aggregator (§0) then weights each technique's contribution rather than
counting them equally:

| Technique | Weight | Rationale |
|---|---|---|
| 1 co-change (all) | 1.0 | Baseline; noisy on its own |
| 2 fix-only co-change | **3.0** | Forced together by a defect — empirical contract |
| 4 range-anchored | 1.5 | Tells you *where*, not just that |
| 7 lagged | 1.5 | Direction information |
| 11 churn correlation | 1.0 | Loose coupling; baseline |
| 12 SZZ | **3.0** | Defect propagation evidence — strongest |
| 13 reviewer overlap | 2.0 | Organizational signal |

Pairs flagged `[structural]` (one file textually references the other —
imports, manifest entries, doc links, distinctive path-segment matches) get
their score divided by 2 in the aggregate, since the coupling is already
explicit and the goal is to surface *latent* coupling.

## 12. Defect propagation (SZZ)

For every fix commit, blame the deleted lines on their introducing commit
(via `git blame --line-porcelain` against the parent), then connect the
introducing file to every other file the fix also touched. Edges are counted
across fixes; pairs with count ≥2 are reported.

The intuition (Śliwerski/Zimmermann/Zeller): if introducing file A is
repeatedly fixed alongside file B, then a change to A is empirically likely
to require a corresponding change to B to avoid a regression.

Cost: bounded to the first 50 fix commits per run. Increase by editing
mine.mjs if your repo has many high-quality fix commits.

## 13. Reviewer overlap

Optional, requires `gh` CLI authenticated against the host. Pulls the last
200 merged PRs, builds a `file → set<reviewer>` index, and flags pairs with
jaccard ≥0.5 and ≥2 shared reviewers.

Reviewer overlap is a strong organizational signal: the people who maintain
this pair already think of them as one unit. If they are also coupled by
co-change but *not* by structural reference, the contract exists in their
heads — write it down.

Skipped automatically if `gh` is missing, unauthed, or `--no-gh`.

## Tuning notes

- **Empty §2 (fixes)**: set `--fix-regex='your|vocabulary|here'`. Without fix
  commits, sections 2 and 12 produce nothing.
- **Empty §3/§9 (range, cross-language)**: thresholds may be too high for a
  small window. Lower `--min-support`, or widen `--since`.
- **Too much output in §1**: raise `--min-support` and `--min-confidence`, or
  use `--top-percent=10` to keep only the head of each list.
- **Slow runs**: lower `--window`, raise `--max-commit-files` (drops mega-
  commits), or add `--skip=12,13` to disable SZZ and reviewer overlap.
- **Buffer overflow on huge histories**: not possible since the rewrite —
  `mine.mjs` streams git log incrementally. If you see one, file a bug.
