# Creating a mesh

## Should this be a mesh?

A mesh names an **implicit semantic dependency**: a coupling between anchors (line-range anchors or whole-file anchors), in code or prose, that is real, that the developer at one anchor needs to know about when touching the other, and that no schema, type, or test already enforces. The standing question at commit time: *did this change create or rely on a coupling that isn't visible from the lines themselves?*

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
- The anchors would just be a note to self better written as a commit message or PR comment.

## Naming

Kebab-case slug that names the *relationship*, not either side. Prefer a **hierarchical path** of kebab-case segments separated by `/`: `<category>/<subcategory>/<identifier-slug>`. Each segment is lowercase a-z, 0-9, and `-`, and starts with a letter or digit. The leaf slug should still fit if either anchor is rewritten.

Recommended shapes, in order of growing scope:

- `<identifier-slug>` — small repo, one obvious domain. `checkout-request-flow`.
- `<category>/<identifier-slug>` — repo spans a few domains. `billing/checkout-request-flow`.
- `<category>/<subcategory>/<identifier-slug>` — recommended default once a category contains more than a handful of meshes, or when the middle segment carries a stable id (an ADR number, a wiki article slug, a threat-model id, a sub-team). `billing/payments/checkout-request-flow`, `auth/oauth/token-refresh`, `adr/0017/uuidv4-lex-order`, `wiki/world-war-ii/eastern-front`, `security/threat-model/t-07-controls-link`.
- Deeper paths are allowed when the hierarchy is real, but stop once another segment stops adding grouping value — flat-but-descriptive beats deep-and-redundant.

Choosing segments:

- **Leaf (identifier-slug).** The noun phrase a person would naturally use to refer to the subsystem the anchors form: `checkout-request-flow`, `tier-rollout`, `rate-limits`, `auth-token`.
- For anchors that form a thing together, name what they form: `checkout-request-flow`, `tier-rollout`, `auth-token`, `rate-limits`.
- For one side that promises or governs the other, name the contract or rule: `charge-request-contract`, `uuidv4-lex-order`, `p1-payment-runbook`.
- For prose-to-prose citations or summaries, name what's being kept in sync: `architecture-summary-sync`, `threat-model-controls-link`.
- **Category / subcategory.** Domain or team at the top (`billing/`, `platform/`, `experiments/`, `docs/`, `auth/`, `wiki/`, `adr/`, `security/`); a stable sub-grouping in the middle (a feature area, a numbered artifact, an article slug, a sub-team). Reuse the same prefix across sibling meshes so prefix searches (`git mesh list --search 'billing/payments/'`) group naturally.
- Avoid naming after one anchor (`charge-ts-deps`, `adr-0017-impl`); the leaf should survive a rename or rewrite of either side.
- Avoid `misc`, `john-work`, `temp`, `frontend`, and other catch-all segments at any level.
- One relationship per mesh. If anchors split into two reasons to change together, create two meshes — typically siblings under the same prefix.

## Writing the why

Write the **why** as a definition: name the subsystem, flow, or concern the anchors collectively form, and say plainly what it does across them. Leave invariants, caveats, ownership, and review triggers to source comments, commit messages, CODEOWNERS, and PR descriptions. The why is inherited across routine re-anchors; only stage a new one when the subsystem itself changes.

```bash
# GOOD — names the subsystem; evergreen, readable out of context
git mesh why billing/checkout-request-flow -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git mesh why experiments/tier-rollout      -m "Tier-rollout bucketing that steers both the live dashboard and the nightly recomputation onto one treatment per user."

# BAD — restates the diff, scolds the reader, or bundles metadata
git mesh why billing/checkout-request-flow -m "Checkout posts the shape api/charge.ts parses."              # describes the coupling, not the subsystem
git mesh why billing/checkout-request-flow -m "Don't change amount without updating the server."            # caveat — belongs in a code comment
git mesh why billing/checkout-request-flow -m "Charge flow. Owner: team-billing. Review on body changes."   # metadata — belongs in CODEOWNERS / PR
```

Re-anchor after drift; do not rewrite the why:

```bash
# Same (path, extent), bytes changed: re-add is a re-anchor (last-write-wins)
git mesh add billing/checkout-request-flow api/charge.ts#L30-L76

# Different line span: rm the old, add the new
git mesh remove  billing/checkout-request-flow api/charge.ts#L30-L76
git mesh add billing/checkout-request-flow api/charge.ts#L34-L82
```

## Line-range anchor vs whole-file anchor

- **Line-range anchor (`path#Lstart-Lend`)** — Default for source code. Points a reviewer at the exact bytes. 1-based, inclusive. A Markdown section, an ADR clause, a contract paragraph, a runbook step are all valid line-range anchor targets.
- **Whole-file anchor (`path` alone)** — The file is consumed as a unit by name or identity. Use for: binaries, images, symlinks, submodule roots, generated/minified assets, **and prose documents whose identity is the contract** — a license, a one-page ADR, a published RFC.

**Recommended default for prose meshes is a whole-file anchor.** Line-range anchors on prose work mechanically but drift noisily under editorial churn (heading renumbers, prettier reflow, sentence rewrites that preserve meaning). Use line-range anchors on prose only when the document has stable structural landmarks (numbered ADRs, contract clauses, threat-model items with stable IDs) *and* the team is willing to re-anchor on editorial passes. See `./responding-to-drift.md` for the chattier prose-drift workflow. See also `./whole-file-and-lfs.md`.

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

When the relationship already exists in history:

```bash
git mesh add auth/token-contract \
  packages/auth/token.ts#L88-L104 \
  packages/auth/crypto.ts#L12-L40
git mesh why auth/token-contract -m "Token verification depends on signature verification."
git mesh commit auth/token-contract
```

Use `--at <commit-ish>` (any ref, tag, or SHA) only when the anchor should be a specific historical commit other than the current one.

## First-commit requirements

A new mesh has no parent to inherit a why from. The first `git mesh commit <name>` fails if no why is staged. Set one with `git mesh why <name> -m "..."` before committing.
