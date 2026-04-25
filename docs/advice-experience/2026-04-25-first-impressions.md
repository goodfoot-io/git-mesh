# `git mesh advice` — first-impressions review

Method: created a real mesh in a scratch repo (`billing/checkout-flow`
spanning `a.ts#L1-L5` and `b.ts#L1-L5`), edited one anchored side,
observed the flush; then ran a no-op repeat and an edit on an
unrelated file. Read against `docs/advice-dx-goals.md`.

## Where it earns its place

- **Goal #1 (partner is the news).** When something fires, the output is
  the *other* file's range + excerpt, not a recap of what I just typed.
  Mesh name and *why* lead the finding.
- **Goal #4 (drift as routing, not alarm).** No "warning:", no severity,
  no color. Reads as "here is the related code," not "you broke
  something."
- **Goal #7 (no repeats).** A second flush of the same write was silent.
- **Goal #9 (fail closed).** Editing an unrelated file produced no
  output and exit 0.
- **Goal #10 (generic).** `#`-prefixed plain text — drops cleanly into a
  terminal, log, or diff.
- **Goal #2 (zero mesh knowledge).** A first-time reader can decode
  `<name> mesh: <why>` + a partner address + an excerpt with no priors.

## Where it strains against the goals

- **Goal #5 (detail scales with certainty) feels inverted on small
  findings.** A one-line edit yielded ~22 lines of preamble and how-to
  wrapping ~8 lines of actual partner content. The ladder is supposed
  to climb with confidence; here the tutorial is constant and the
  finding is the small bit at the bottom. Defensible on the very first
  run of a session; corrosive as steady state.
- **Goal #6 (teach alongside a recommendation) is partly bypassed.** The
  flush teaches `git mesh show / ls / stale` and the `add … / commit`
  re-record pattern even though *no* concrete recommendation is being
  made — this is routing-only (L1 partner). Teaching is pre-emptive,
  not "alongside the recommendation it supports." Right behavior:
  surface the concept block the moment a candidate-promotion or
  stale-repair action lands, and stay absent otherwise.
- **Goal #3 (consequences, not prescriptions) is on the edge.** "Compare,
  then either update it or accept that the relationship has shifted and
  re-record the mesh" leans into intent and what-to-do. The partner
  address + excerpt alone already say it.
- **Goal #1 (minimum context that makes the partner legible) is
  under-served on the developer's side.** The output never says *which*
  range in `a.ts` moved or shows the trigger excerpt. Fine for a 5-line
  file; for a 2000-line file the partner is legible but there is no
  quick handle on which slice of my own edit triggered the routing.
- **Address echo.** The partner appears as a bullet (`# - b.ts#L1-L5`)
  and again as a heading above the excerpt (`# b.ts#L1-L5`). One is
  slack ink.
- **Header ordering.** The mesh `name: why` line — load-bearing identity
  per Goal #9 — sits *below* the preamble. A skim hits prose before
  signal.

## Net subjective take

Hits the structural commitments — partner-led, silent when uncertain,
no severity affect, repeat-suppressed, plain text. Misses on
**economy**: the teaching/finding ratio inverts on the small-finding
case the developer will see most often, and the prescriptive verbs in
the standing preamble blur Goal #3. Tunings that would close the gap:

1. Gate the concept block strictly to first-time *recommendation*
   surfacings, not first-time routing.
2. Lead with `mesh: why`, then partner, then any teaching.
3. Include a one-line trigger locator on the developer's side so the
   partner has somewhere to anchor.

## Would it be useful for development today?

Conditionally yes, with caveats.

**Useful when:**

- The repo has meshes that genuinely encode latent contracts (parallel
  data shapes, twinned client/server flows, paired migrations). In
  those cases the partner-led routing is exactly the read a developer
  would have done by hand, delivered automatically.
- The session is long-running and edits cluster around a few meshes.
  The repeat-suppression (Goal #7) means the noise floor stays low and
  each new finding actually means something.
- The team treats meshes as evergreen subsystem definitions (per the
  CLAUDE.md guidance) rather than as alarms. Advice composes that
  shape; it doesn't fight it.

**Less useful when:**

- The repo has no meshes or only a few. The cost of the per-flush
  preamble shows up against zero or one finding and the signal-to-ink
  ratio inverts. A developer running it speculatively will mostly see
  silence (good) punctuated by tutorial-shaped output (less good).
- The developer is doing wide-area refactors. The concept block fires
  on the first finding of every fresh session and the teaching is the
  same teaching they saw yesterday. Goal #7's "don't repeat" is
  per-session, not per-developer.
- The team has not internalized that mesh *whys* are evergreen
  definitions. If whys read as caveats or task notes, advice will
  faithfully surface that drift across the team and feel like nagging
  even though the command is doing exactly what it promises.

**Bottom line.** As a routing aid for a developer mid-edit in a
mesh-rich repo, advice does the right thing in the right shape and is
worth keeping in the loop. As a general daily-driver across a typical
codebase, it currently spends too much ink teaching for the volume of
findings it has to teach about; the mechanism is right and the
calibration is off. Tighten the teaching gate (Goal #6) and the
preamble economy (Goal #5) and it crosses from "interesting" to
"earns its place after every edit," which is the bar the goals doc
itself sets.
