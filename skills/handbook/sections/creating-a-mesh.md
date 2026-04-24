# Creating a mesh

## Should this be a mesh?

Mesh relationships that are real but not already mechanically enforced.

Good candidates:
- Request construction in a client and request parsing in a server
- A schema definition and handwritten consumers of it
- A feature flag declaration and the code that interprets the flag
- A permissions rule and tests that prove the rule
- A public API and documentation that promises its behavior
- A migration, validation rule, and rollback code
- Two implementations of the same invariant in different languages

Skip when:
- **A type system, schema, validator, or test already enforces it.** Use that instead — it rejects violations automatically.
- A build step regenerates one side from the other perfectly.
- The ranges would just be a note to self better written as a commit message or PR comment.

## Naming

Kebab-case slug that names the subsystem, optionally prefixed by a category: `<category>/<slug>`. Pick the noun phrase a reviewer would naturally use for the thing the ranges form — `checkout-request-flow`, `tier-rollout`, `auth-token`, `rate-limits`.

- Add a category prefix (`billing/`, `platform/`, `experiments/`, `docs/`, `auth/`) when the repo spans multiple domains or teams.
- Skip the prefix when the area is obvious.
- Avoid `misc`, `john-work`, `temp`, `frontend`.
- One relationship per mesh. If ranges split into two reasons to change together, create two meshes.

## Writing the why

Write as a definition: name the subsystem, flow, or concern the ranges collectively form, and say plainly what it does across them.

```bash
# GOOD — names the subsystem, evergreen, readable six months from now
git mesh why billing/checkout-request-flow \
  -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."

# BAD — restates the diff, scolds, or bundles metadata
git mesh why billing/checkout-request-flow -m "Checkout posts the shape api/charge.ts parses."
git mesh why billing/checkout-request-flow -m "Don't change amount without updating the server."
git mesh why billing/checkout-request-flow -m "Charge flow. Owner: team-billing. Review on body changes."
```

Leave invariants, caveats, ownership, and review triggers to source comments, commit messages, CODEOWNERS, and PR descriptions. The why is about the relationship alone.

## Line range vs whole file

- **Line range (`path#Lstart-Lend`)** — Default. Points a reviewer at the exact bytes. 1-based, inclusive.
- **Whole file (`path` alone)** — Use when the file has no meaningful line structure: binaries, images, symlinks, submodule roots, generated/minified assets. See `./whole-file-and-lfs.md`.

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
