# Writing meshes

You're adding a mesh to the dependency database. The mesh fires when someone reads, edits, deletes, or renames the lines you anchor — in code or prose. Your job is to give the future developer enough to recognize what kind of coupling they're touching, no more.

This applies in two situations:

1. **Active edits.** You're committing a change. Anchor the mesh in the same commit that creates the coupling.
2. **Existing code or docs.** You're reading or maintaining material that already contains a coupling no one has anchored. Anchor it now, against the current ranges (see *Anchoring against existing code*). Don't wait for a hypothetical future commit to "make it relevant."

You're not documenting your code. You're documenting **what the off-screen world does to it**, or what one anchor relies on the other anchor to stay true. If a region is internally self-explanatory and has no external consumers or load-bearing reader, no mesh.

## What to mesh

Ask: *did this region create or rely on a coupling that isn't visible from the lines themselves?* If a literal you wrote is parsed by something else as a string, if a function name you defined is read by configuration, if an order you established is required by a downstream consumer, if a sleep is masking something, if a doc states a body shape a parser honors, if a runbook step is followed by a responder under deadline — that's a mesh.

The sharpest test: **if you can't name the wrong decision a reader of one anchor would make under deadline when the other changes silently, it's a link, not a mesh.** "Read this when changing X" is not a wrong decision; "ship a broken integration," "mishandle an incident," "violate the contract" are.

Anchors can be code or prose. A Markdown section, an ADR clause, a contract paragraph, a runbook step are first-class anchor sites alongside source files.

## How to anchor

Pick the **narrowest range** that still fires when someone touches the dependency:

- A single line for a literal, a sleep, a flush, a function signature, a contract clause
- A function-body or section-body range when the dependency is the behavior or claim across that scope
- A whole-file ref only when the file is consumed *as a unit* — its bytes-as-a-whole are the thing the partner depends on. Whole-file fires on every commit touching the file, so use it sparingly for code, but it is the **recommended default for prose** (license, one-page ADR, published RFC, signed MSA, SOC2 narrative). Line-ranged prose drifts noisily under editorial churn; reach for it only when the document has stable structural anchors and the team accepts re-anchoring on editorial passes.

Always add the **partner range** pointing at the consumer or the side that makes the coupling matter. The partner range is often more valuable than the why's prose, because the developer clicks through and sees the dependency for themselves. A mesh with one anchor is rarely the right shape — if you can't name a partner, you may be writing a comment, not a mesh.

## How to write the why

The why is **one prose sentence naming the relationship the ranges hold**, written so it survives a rewrite of either side.

Style rules that apply to every why:

- **Prose, not log entries.** No leading keyword like `contract:`, `spec:`, `note:`, `fix:`. The mesh name carries the label; the why is documentation.
- **Role-words, not filenames.** The ranges already carry the paths. The why says "the doc," "the parser," "the runbook," "the responder," "the migration," "the client." A path appears in the why only when the path *itself* is what the partner depends on (a hard-coded script reference, a generated file referenced by name in CI). Repeating a filename the ranges already carry locks the why to current names — a rename then invalidates the prose along with the anchor.
- **Sharpen role-words when one side isn't enough.** The point is disambiguation, not minimalism. When both anchors share a role-word — both prose ("the doc"), both code ("the handler"), both runbooks — reach for sharper role-words ("the threat entry" / "the paired control," "the request doc" / "the parser," "the runbook step" / "the alert handler") before falling back to filenames. If one role-word genuinely covers both sides, the why isn't specific enough yet.
- **State what is, not what could go wrong.** "The doc states the body shape the parser honors" is correct. "Don't rename this or the parser breaks" is the wrong shape. The developer infers the failure mode from their own intent. Phrases like *racy*, *invisible*, *silently breaks*, *will fail* leak knowledge of outcomes you may not actually have and narrow the protection to one failure mode.
- **Name asymmetry in the prose, not as a prefix.** Promises and governance dependencies are directional. Use clauses like "the doc is the source of truth when they disagree," "the spec promises the shape the parser honors," "the ADR governs the assumption the sort relies on." This is more honest than a `Specification:` prefix because it forces you to name *which* side is normative.
- **No bundling.** A why naming three things ("strict parser, field names load-bearing, owner: billing") usually means the mesh is trying to carry more than one relationship and should be split. Ownership and review triggers belong in CODEOWNERS and PR descriptions.
- **Use convention markers only when a fact statement won't push hard enough.** *load-bearing*, *do not remove*, *order matters*, *wire format*, *referenced by name*, *behavior contract*. A flush that looks pointless needs *load-bearing — the worker reads the bytes after the handler returns*. The fact alone wouldn't push hard enough; the marker does.

If you're stuck, reach for one of these framings — vocabulary, not categories you have to pick before starting:

- **Subsystem** — symmetric co-implementation: the ranges *together* form a thing. *"Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."*
- **Specification** — asymmetric: one side is the source of truth, the other must conform. Covers promises, governance, normative references, ADRs that govern code, contracts that bind code. *"The doc states the request body shape the parser honors; the doc is the source of truth when they disagree."*
- **Mechanism** — the dependency *is* a non-obvious mechanism the lines don't show: a load-bearing flush, an import-time side effect, a sleep masking a race, dynamic name construction. *"Load-bearing — the worker reads this file after the handler returns."*
- **Consumer role** — a downstream depends on these lines in a specific way: a binding regen target, a CDC tail, a literal client on a slow release cycle. *"Binding regen target — the Python and Go clients are regenerated from these signatures."*
- **Contract** — the dependency is on a property the code or doc maintains rather than on another file: a sort order a `bisect` reads, a UUID lex-order convention, a schema-migration requirement. *"Items are read as sorted by timestamp ascending; downstream cross-service joins rely on the order."*

## Naming

Kebab-case slug naming the *relationship*, not either side, optionally prefixed by a category: `<category>/<slug>`. The slug should still fit if either anchor is rewritten.

- For ranges that form a thing together, name what they form: `checkout-request-flow`, `tier-rollout`, `auth-token`, `rate-limits`.
- For one side that promises or governs the other, name the contract or rule: `charge-request-contract`, `uuidv4-lex-order`, `p1-payment-runbook`.
- For prose-to-prose citations or summaries, name what's being kept in sync: `architecture-summary-sync`, `threat-model-controls-link`.
- Avoid naming after one anchor (`charge-ts-deps`, `adr-0017-impl`); the slug should survive a rename or rewrite of either side.
- Avoid `misc`, `temp`, `frontend`.
- Load up the *name* with the label so the why doesn't have to. If the why's first words restate the slug, drop them.
- One relationship per mesh.

## Anchoring against existing code

When the coupling already exists in history and you're meshing it now rather than at the moment of creation:

```bash
git mesh add <category>/<slug> \
  <path>#L<start>-L<end> \
  <partner-path>#L<start>-L<end>
git mesh why <category>/<slug> -m "<one prose sentence>"
git mesh commit <category>/<slug>
```

Use this when reading or maintaining existing material — you don't need to wait for an active edit. Skim the surrounding region first to confirm it's actually load-bearing and the partner is path-addressable; if not, it's a link, not a mesh. Pass `--at <ref|SHA>` only when the anchor should be a specific historical commit rather than the current one.

## What not to mesh

- **Things visible from the line itself.** If the developer opens the file and sees the dependency immediately, the mesh is redundant. The path+range alone is the warning.
- **Failure modes you anticipate but haven't observed.** If you're guessing what someone might do wrong, you're narrowing the protection. State the coupling as it exists; let the developer derive their risk.
- **Local code structure or style.** Meshes are for cross-cutting couplings, not for "this function is complex" or "consider refactoring."
- **Purely descriptive prose against code that is itself the source of truth.** A tutorial paragraph, a README walkthrough, a code-comment paraphrased into a doc — none of these are load-bearing. Reach for a mesh only when someone reads the prose and acts on it under stakes.
- **Things that aren't path-addressable.** Production data shape, external service config, runtime database state — if there's no path in the repo where the dependency lives, the mesh can't anchor and doesn't belong here. Document those somewhere with a different shape.
- **Things a type, schema, validator, or test already enforces.** Use that mechanism — it rejects violations automatically.

## Read sensibly across all interactions

The mesh fires when someone opens, edits, deletes, or renames either anchor. Test your draft against each: would a reader, an editor, a deletor, and a renamer all read your why as informative? If it only makes sense to the editor (e.g. only addresses "what's risky to change"), redraft. The why has to orient a reader, justify itself to a deletor, and survive a rename without going stale.

## A useful self-check

Before you commit the mesh, ask: *if a developer who has never seen this codebase before clicks through to the partner anchor I'm referencing, will they understand the coupling within ten seconds?* If yes, the mesh is doing its job. If they'd need to read the why two or three times and then go hunting for context, the mesh is carrying too much load — either anchor a better partner, sharpen the role-words in the why, or accept that this dependency isn't really expressible as a mesh and document it elsewhere.
