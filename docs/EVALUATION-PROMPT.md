You are evaluating the developer experience of `git mesh advice`, a
Claude Code hook-driven command active in this session. Your audience
is the maintainer tuning the command. The output of this evaluation
is used to refine the command's behavior — be specific, be opinionated,
and ground every claim in something you actually saw on stdout.

You are the developer at the prompt, not an auditor with a checklist.
Lead with what you *felt* before what you *concluded*. The goals
document exists, but you will not consult it until the end.

### What you may read

- The implementation under `packages/git-mesh/src/advice/` if you need
  to confirm a behavior you observed (read sparingly; the evaluation
  is about output, not code review).
- The hook scripts under `plugins/git-mesh/bin/` to understand when
  advice fires.
- `docs/advice-dx-goals.md` — the ten experience commitments. **Do
  not open this until step 4 of "How to evaluate" below.** The
  rubric is for cross-checking your reactions, not for seeding them.
- Prior evaluations in `docs/advice-experience/` — only after you have
  drafted your own findings, to avoid anchoring.

### What you must NOT read

- `docs/advice-notes.md` (the planning doc). Skip it entirely.
- Any internal design or rationale doc beyond the goals doc above.

### Concepts you need (do not look these up — they are stated here)

**What a mesh is.** A mesh is a named, durable contract over line
ranges across files in a repo. It carries a *why* — a short,
evergreen sentence that names the subsystem, flow, or concern those
ranges collectively form (e.g. `billing/checkout-flow`: "Checkout
request flow that carries a charge attempt from the browser to the
Stripe-backed server."). A mesh is *not* a warning, not a TODO, not
ownership metadata. The why is load-bearing identity, not commentary.

**Core mesh reads** advice composes:
- `git mesh ls` — meshes that touch a file
- `git mesh stale` — ranges whose anchored bytes have drifted
- `git mesh why <name>` — read the why
- `git mesh <name>` — show ranges, why, config

**Re-anchor pattern.** Same `(path, extent)`, content drifted →
`git mesh add <name> <path>#L<s>-L<e>` is a last-write-wins re-record.
Different line span → `rm` the old, `add` the new.

**What advice is.** A session-scoped command that consumes write
events (Edit/Write/NotebookEdit hooks) and flushes routing-shaped
output: "given what just moved, which mesh ranges deserve attention
right now, and what does the mesh's why say about why I should care?"
Every line could in principle have been reconstructed by hand from
the reads above; advice's job is timing, ordering, and suppression.

### How to exercise it

You must produce **at least three independent runs** in a scratch
repo (`/tmp/<your-name>-advice-N`), covering different conditions.
Required scenarios; add more if you find interesting edges:

1. **First-finding routing.** New session, fresh mesh, edit one
   anchored range. Capture full flush output.
2. **Repeat suppression.** Same session, same edit re-applied.
3. **Unrelated edit.** Same session, edit a file no mesh touches.
4. **Multi-mesh / cross-cutting.** A file under two meshes, or a
   staged change that overlaps another mesh's range.
5. **Stale promotion.** Edit drives a range past the stale threshold
   so a re-record recommendation fires.
6. **Whole-file / deleted / submodule partner** if you can construct
   one — these have address-only rendering paths.

For each run, record:
- The exact setup commands.
- The trigger edit.
- The full flush output, verbatim, **on the first occurrence**;
  thereafter, if the preamble or any large block repeats byte-for-
  byte, refer to it as `<preamble>` (or another short label) rather
  than re-quoting. Re-quoting inflates length without adding signal.
- One-sentence note on what the run was meant to probe.

Use `git mesh advice <session-id>` directly when you need to flush
without going through the Edit tool. Use distinct session ids per
scenario so repeat-suppression state is clean.

**Before flushing run N+1 (for N ≥ 1), write down what you expect
to see** — one or two sentences, prediction only, no hedging. After
the flush, note whether you were surprised. Surprise is the signal.

### How to evaluate

Do these in order. Do not skip ahead.

**Step 1 — Gut reactions, run by run.** For each run, before any
analysis, write:

- **Felt:** one sentence. What was your immediate reaction reading
  the flush — relief, confusion, boredom, suspicion, "huh"? Name the
  feeling, not the cause.
- **Paraphrase test:** close the output. From memory, write — in
  one sentence — what the news was. Then re-open and compare. Note
  any gap between what you remembered and what was actually said;
  the gap is real DX signal that quoting alone cannot recover.
- **Keep it on?** Binary: keep / mute. One sentence of reasoning.
- **Reminds me of:** does this output read more like a linter, a
  code-review comment, a `git status`, a changelog, a chat message,
  a man page, a test failure, a tooltip? Pick one (or coin one) and
  say why in a clause.
- **Vocabulary check:** list every term in the output you could
  not define from the output alone. Be honest — terms that the
  surrounding documentation explains do not count.
- **Silence cases (runs that produce empty output):** describe
  how the silence felt — reassuring, suspicious, invisible? Empty
  output is a designed behavior; treat it like any other output.

**Step 2 — One sample rewrite.** Pick the single run whose flush
bothered you most. Rewrite the flush as you would prefer it,
preserving the same finding. Not a tuning suggestion in prose — an
actual alternative output. Falsifiable; reveals taste the rubric
cannot.

**Step 3 — Forced ranking.** Across all your runs, pick:

- **Delete-first line:** the single line you would delete first,
  and one sentence on why.
- **Fight-to-keep line:** the single line you would defend hardest,
  and one sentence on why.
- **Metaphor:** finish the sentence: *`git mesh advice` is like ___
  that ___.* One sentence, no more.

**Step 4 — Rubric cross-check.** *Now* open
`docs/advice-dx-goals.md`. For each of the ten goals, write:

- **Verdict**: met / partially met / missed.
- **Evidence**: a quote (≤6 lines) from one of your runs that
  supports the verdict. Cite the run number.
- **Mechanism**: one sentence on *why* the output behaves this way
  (e.g. "the preamble fires on first finding regardless of whether
  a recommendation is being made").
- **Tuning**: a concrete change to the output that would move the
  verdict toward "met" without violating other goals.

Where a verdict in step 4 contradicts a feeling in step 1, **say so
explicitly** and resolve which one you trust. Subjective and
goal-driven readings disagreeing is the most useful finding the
evaluation can produce.

**Step 5 — Cross-cutting section.** Cover at least:

- **Economy.** Lines of teaching vs. lines of finding, per run. Is
  the ratio defensible at steady state?
- **Trigger legibility.** Does the developer know which range of
  *their own* edit caused the routing? If not, what is the cheapest
  fix that would not violate Goal #1 ("news is the partner")?
- **Header ordering.** Does the load-bearing identity (`<name> mesh:
  <why>`) lead, or does prose lead?
- **Tone.** Any prescriptive verbs ("compare," "update," "accept")
  that nudge intent against Goal #3? Quote them.
- **Steady-state feel.** Imagine 20 flushes across a workday in a
  mesh-rich repo. What goes from useful to noisy first?
- **Prediction accuracy.** Across the predictions you wrote in step
  "before flushing N+1," which matched and which surprised? Patterns
  in the surprises are model-of-the-tool gaps worth naming.
- **Failure modes you saw.** Anything broken, surprising, or
  contradictory across runs.

**Step 6 — Framing question.**

> **Is `git mesh advice` useful for development today?** Under what
> conditions yes, under what conditions no. What is the single change
> that would most improve its DX for a developer (or Claude) running
> it after every edit?

### Output

Write findings to:

- `docs/advice-experience/<YYYY-MM-DD>-<short-slug>.md`

The file should contain, in order:

1. One-paragraph method note (how many runs, what scenarios).
2. **Step 1 reactions** — gut/paraphrase/keep-it-on/reminds-me-of/
   vocabulary/silence, run by run.
3. **Step 2 sample rewrite** of one flush.
4. **Step 3 forced ranking** — delete-first, fight-to-keep,
   metaphor.
5. **Step 4 rubric cross-check** — per-goal verdict / evidence /
   mechanism / tuning, plus any contradictions with step 1.
6. **Step 5 cross-cutting section.**
7. **Step 6 verdict on usefulness + single highest-leverage change.**
8. Appendix: each run's setup, trigger, prediction (if any), and
   verbatim flush output (preamble quoted once, then labelled).

Length budget: 600–1200 lines is fine if the runs are real and the
quotes are tight. Do not pad. Do not re-quote the preamble.

### Rules

- Quote actual output. Do not paraphrase what advice "would" say.
- Quote any preamble or repeated block **once**; thereafter use a
  short label (`<preamble>`).
- If a goal cannot be exercised because no scenario triggers it, say
  so explicitly rather than guessing.
- Do not recommend changes that violate other goals; if a tuning is
  in tension with another goal, name the tradeoff.
- Do not open `docs/advice-dx-goals.md` until step 4. Reading it
  earlier defeats the purpose of the subjective pass.
- Do not read `docs/advice-notes.md`. The point is fresh eyes against
  the public goals doc.
- Do not edit advice's source code as part of the evaluation.
- Do not create migrations or backwards-compat shims if you do
  prototype a tuning in a separate branch later (per repo CLAUDE.md).
