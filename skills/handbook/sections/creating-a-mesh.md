# Creating a mesh

## Should this be a mesh?

A mesh names an **implicit semantic dependency**: a coupling between line ranges (or whole files), in code or prose, that is real, that the developer at one anchor needs to know about when touching the other, and that no schema, type, or test already enforces. The standing question at commit time: *did this change create or rely on a coupling that isn't visible from the lines themselves?*

The sharpest test: **if you can't name the wrong decision a reader of one anchor would make under deadline when the other changes silently, it's a link, not a mesh.** ("Read this when changing X" isn't a wrong decision; "ship a broken integration," "mishandle an incident," "violate the contract" are.)

Good candidates (a deliberate mix of code↔code, code↔prose, and prose↔prose):
- Request construction in a client and request parsing in a server
- A documented API request shape and the handwritten parser that honors it (`docs/api/charge.md` ↔ `api/charge.ts`)
- An ADR that governs a runtime assumption and the code that relies on it (`docs/adr/0017-uuidv4.md` ↔ `services/joiner/sort.ts`)
- A runbook step a responder follows under pressure and the alert handler that emits the alert
- A contract clause and the billing code that performs against it
- A threat-model item and the control doc that mitigates it (`docs/security/threat-model.md§T-07` ↔ `docs/security/controls.md§C-12`)
- A feature flag declaration and the code that interprets the flag
- An ordering or sort invariant maintained at one site and relied on at another
- A regen target and the source it regenerates from
- A load-bearing flush, sleep, or import-time call whose role isn't visible locally

Skip when:
- **A type system, schema, validator, or test already enforces it.** Use that instead — it rejects violations automatically.
- A build step regenerates one side from the other perfectly and the regen is not itself the dependency.
- **The prose is purely descriptive of code that is itself the source of truth** — a tutorial paragraph, a README walkthrough, a code-comment paraphrased into a doc. Reach for a mesh only when the prose is *load-bearing*: someone reads it and acts on it (a contract, a normative spec, a published API promise, a runbook a responder follows under pressure).
- The dependency isn't path-addressable (production data shape, external service config, runtime db state). Document those somewhere with a different shape.
- The ranges would just be a note to self better written as a commit message or PR comment.

## Naming

Kebab-case slug that names the *relationship*, not either side, optionally prefixed by a category: `<category>/<slug>`. The slug should still fit if either anchor is rewritten.

- For ranges that form a thing together, name what they form: `checkout-request-flow`, `tier-rollout`, `auth-token`, `rate-limits`.
- For one side that promises or governs the other, name the contract or rule: `charge-request-contract`, `uuidv4-lex-order`, `p1-payment-runbook`.
- For prose-to-prose citations or summaries, name what's being kept in sync: `architecture-summary-sync`, `threat-model-controls-link`.
- Avoid naming after one anchor (`charge-ts-deps`, `adr-0017-impl`); the slug should survive a rename or rewrite of either side.
- **Load the label into the name, not the why.** A descriptive slug (`charge-request-contract`, `uuidv4-lex-order`, `p1-payment-runbook`) lets the why be plain prose about the relationship instead of having to label itself.
- Add a category prefix (`billing/`, `platform/`, `experiments/`, `docs/`, `auth/`) when the repo spans multiple domains or teams.
- Avoid `misc`, `john-work`, `temp`, `frontend`.
- One relationship per mesh. If ranges split into two reasons to change together, create two meshes.

## Writing the why

**One rule:** name the relationship the ranges hold in one prose sentence, written so it survives a rewrite of either side.

The reader opens the files to see the mechanism for themselves; the why's job is to orient them, not to pre-chew the answer. Three style rules follow:

- **The mesh name carries the label.** The why is prose; don't restate the name as a prefix or use a git-style leading keyword (`contract:`, `spec:`, `gov:`, `note:`). If the why's first words restate the name, drop them.
- **The ranges carry the paths.** Describe the relationship in role-words — "the doc," "the parser," "the client," "the runbook," "the responder," "the migration" — rather than repeating filenames. A why without filenames survives a rename. Name a path only when the path itself is part of the dependency (a hard-coded script reference, a generated file invoked by name). External proper nouns the ranges don't carry (vendor names like `Stripe`, system names like `Kafka`) are fine.
- **For asymmetric relationships, name which side is normative in prose.** "The doc is the source of truth when they disagree." "X promises the shape Y honors." "X governs the assumption Y relies on." Don't smuggle this in as a category prefix.

Avoid restating the diff, embedding incidental implementation properties (parser strictness, current field names), scolding, or bundling ownership and review triggers — those belong in source comments, commit messages, CODEOWNERS, and PR descriptions.

```bash
# GOOD — names the relationship in prose, no filename repetition, survives rewrites
git mesh why billing/checkout-request-flow \
  -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."

git mesh why billing/charge-request-contract \
  -m "The doc states the request body shape the parser honors; the doc is the source of truth when they disagree."

git mesh why platform/uuidv4-lex-order \
  -m "An ADR governs the v4 lex-order assumption the joiner relies on for cross-service joins."

# BAD — restates the name as a prefix, repeats filenames already in the ranges, embeds incidental implementation, scolds, or bundles metadata
git mesh why billing/charge-request-contract -m "Contract: docs/api/charge.md states the body shape api/charge.ts parses."
git mesh why billing/charge-request-contract -m "docs/api/charge.md states the body shape api/charge.ts parses."
git mesh why billing/checkout-request-flow -m "Browser POST body — strict parser, field names load-bearing."
git mesh why billing/checkout-request-flow -m "Don't change amount without updating the server."
git mesh why billing/checkout-request-flow -m "Charge flow. Owner: team-billing. Review on body changes."
```

### Vocabulary, when you're stuck

The "name the relationship" rule is enough most of the time. If you're stuck, try one of these framings — they are scaffolding, not categories the writer has to pick before starting:

- **Subsystem** — symmetric co-implementation: the ranges *together* form a thing. *Checkout request flow across client and server.*
- **Specification** — asymmetric: one side is the source of truth, the other must conform. Covers promises, governance, normative references, ADRs that govern code. *`docs/adr/0017-uuidv4.md` governs the lex-order assumption in `services/joiner/sort.ts`.*
- **Mechanism** — the dependency *is* a non-obvious mechanism the lines don't show: a load-bearing flush, an import-time side effect, a sleep masking a race, dynamic name construction (`f"{prefix}_KEY"`).
- **Consumer role** — a downstream depends on these lines in a specific way: a binding regen target, a CDC tail, a literal client on a slow release cycle.
- **Contract** — the dependency is on a property the code maintains rather than on another file: a sort order a `bisect` reads, a UUID lex-order convention, a schema-migration requirement.

Don't bundle several of these into one why ("strict parser, field names load-bearing, owner: billing") — that usually means the mesh is trying to carry more than one relationship and should be split.

## Line range vs whole file

- **Line range (`path#Lstart-Lend`)** — Default for source code. Points a reviewer at the exact bytes. 1-based, inclusive. A Markdown section, an ADR clause, a contract paragraph, a runbook step are all valid line-range targets.
- **Whole file (`path` alone)** — The file is consumed as a unit by name or identity. Use for: binaries, images, symlinks, submodule roots, generated/minified assets, **and prose documents whose identity is the contract** — a license, a one-page ADR, a published RFC.

**Recommended default for prose meshes is whole-file.** Line-ranged prose works mechanically but drifts noisily under editorial churn (heading renumbers, prettier reflow, sentence rewrites that preserve meaning). Use line-ranged prose only when the document has stable structural anchors (numbered ADRs, contract clauses, threat-model items with stable IDs) *and* the team is willing to re-anchor on editorial passes. See `./responding-to-drift.md` for the chattier prose-drift workflow. See also `./whole-file-and-lfs.md`.

## Commit sequence alongside a code change

```bash
git mesh add billing/checkout-request-flow \
  web/checkout.tsx#L88-L120 \
  api/charge.ts#L30-L76
git mesh why billing/checkout-request-flow \
  -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git commit -m "Wire checkout to charge API"   # post-commit hook runs `git mesh commit`
```

`git mesh add` without `--at` snapshots the working tree and resolves the anchor at mesh commit time — which is how the post-commit flow anchors to the source commit that just landed.

## Documenting existing code

When the relationship already exists in history, anchor explicitly:

```bash
git mesh add auth/token-contract --at HEAD \
  packages/auth/token.ts#L88-L104 \
  packages/auth/crypto.ts#L12-L40
git mesh why auth/token-contract -m "Token verification depends on signature verification."
git mesh commit auth/token-contract
```

`--at <commit-ish>` accepts any ref, tag, or SHA. Use it when the anchor should be a specific historical commit rather than the post-commit hook moment.

## First-commit requirements

A new mesh has no parent to inherit a why from. The first `git mesh commit <name>` fails if no why is staged. Set one with `git mesh why <name> -m "..."` before committing.
