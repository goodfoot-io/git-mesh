<golden-rule>
After making any changes to code or configuration files, lint, type check, and run all tests. (Not required for markdown, JSON, or CSS changes.)

異常を検知した時点で、誰もが即時に可視化・共有し、作業を一旦停止して真因を特定し、再発防止策（恒久対策）を講じてから再開する。

This applies to all warnings and failures encountered during validation, not only warnings or failures caused by your changes. Do not dismiss failures as "pre-existing" or "unrelated."

**A test that does not run because of an infrastructure error is a blocking condition.** Do not proceed with implementation.
</golden-rule>

<greenfield>
This is a greenfield implementation. Do not create migrations, backwards compatibility, or fallbacks.
</greenfield>

<right-way-over-easy-way>
Always choose the "right way" over the "easy way".
</right-way-over-easy-way>

<fail-closed>
Prefer 'fail closed' workflows over 'fail open' workflows.
</fail-closed>

<commit-message>
Do not include a "Co-Authored-By: Claude ..." message in commits.
</commit-message>

<workspace-information>
Our workspace uses Yarn 4.x as a package manager. Do not use other package managers such as 'npm'.

This is a Yarn 4.x monorepo with packages in ./packages/ containing a Rust CLI (packages/git-mesh) and a VS Code extension (packages/extension).

Use local rather than origin branches.
</workspace-information>

<jsdoczoom>

**Shows increasing levels of documentation in TypeScript files based on JSDoc annotations.**

```bash
# Use instead of `find . -name "*.ts" | xargs grep -ril "CacheKey|buildIndex|TreeNode"`
jsdoczoom ./src/** --search "CacheKey|buildIndex|TreeNode"
```

Each output header - "# [FILE PATH]@[DEPTH]" - is the next drill-down selector.

Run `jsdoczoom [FILE PATH]@[DEPTH]` to get deeper information on the file.

Then `jsdoczoom [FILE PATH]@[DEPTH + 1]` to get deeper still.

```bash
# The --search value is a regex passed as a plain string — never escape | or other regex metacharacters
jsdoczoom --search "foo|bar"      # GOOD: matches either foo or bar
jsdoczoom --search "foo\|bar"     # BAD: treats \| as a literal character, not alternation
```

Use the `jsdoczoom:jsdoczoom` subagent instead of the `Explore` subagent to answer code questions in this repository.
</jsdoczoom>

<documentation>

Project documentation lives in `README.md` and `docs/`. Keep documentation focused on `git-mesh`, the Rust CLI in `packages/git-mesh`, and the lightweight VS Code extension in `packages/extension`.

</documentation>

<git-mesh>

**A lightweight contract for agreements that no schema, type, or test already enforces.** A mesh anchors line ranges (or whole files) across the repo and carries a durable "why" defining the subsystem they collectively form.

```bash
# Create the mesh while making the code change
git mesh add billing/checkout-request-flow web/checkout.tsx#L88-L120 api/charge.ts#L30-L76
git mesh why billing/checkout-request-flow -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git commit -m "Wire checkout to charge API"   # post-commit hook anchors the mesh
```

Write the **why** as a definition: name the subsystem, flow, or concern the ranges collectively form, and say plainly what it does across them. Leave invariants, caveats, ownership, and review triggers to source comments, commit messages, CODEOWNERS, and PR descriptions. The why is inherited across routine re-anchors; only stage a new one when the subsystem itself changes.

```bash
# GOOD: names the subsystem — evergreen, readable out of context
git mesh why billing/checkout-request-flow -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git mesh why experiments/tier-rollout      -m "Tier-rollout bucketing that steers both the live dashboard and the nightly recomputation onto one treatment per user."

# BAD: restates the diff, scolds the reader, or bundles metadata
git mesh why billing/checkout-request-flow -m "Checkout posts the shape api/charge.ts parses."              # describes the coupling, not the subsystem
git mesh why billing/checkout-request-flow -m "Don't change amount without updating the server."            # caveat — belongs in a code comment
git mesh why billing/checkout-request-flow -m "Charge flow. Owner: team-billing. Review on body changes."   # metadata — belongs in CODEOWNERS / PR
```

Name a mesh with a kebab-case slug that titles the subsystem, optionally prefixed by a category: `<category>/<slug>`. Pick the noun phrase a person would naturally use to refer to the thing the ranges form (`checkout-request-flow`, `tier-rollout`, `rate-limits`, `auth-token`). Add a category prefix (`billing/`, `platform/`, `experiments/`, `docs/`, `auth/`) when the repo spans multiple domains or teams; skip it when the area is obvious.

Re-anchor after drift; do not rewrite the why:

```bash
# Same (path, extent), bytes changed: re-add is a re-anchor (last-write-wins)
git mesh add billing/checkout-request-flow api/charge.ts#L30-L76

# Different line span: rm the old, add the new
git mesh rm  billing/checkout-request-flow api/charge.ts#L30-L76
git mesh add billing/checkout-request-flow api/charge.ts#L34-L82
```

Lean toward creating meshes — they surface drift and cross-file context that nothing else in the repo makes visible. The only agreements to skip are those a compiler, schema, type, or test already enforces: a shared TypeScript type, a Protobuf message, a Zod validator imported by both sides, a contract test. Those mechanisms are strictly better than a mesh over the same surface because they reject violations automatically. Mesh everywhere those tools cannot reach: cross-language reimplementations of the same invariant, docs that promise specific code behavior, assets pinned next to the copy that describes them, client/server boundaries where neither side types the other, config values interpreted by multiple consumers. Prefer line ranges — they point a reviewer at the exact bytes. Use whole-file pins (omit `#L...`) only when the file has no meaningful line structure: binaries, images, submodule roots, generated assets. 
</git-mesh>

<validation>
Run validation from the package directory containing the changed files, using that package's scripts from `package.json` (e.g., `yarn lint`, `yarn typecheck`, `yarn test`).

Always focus test runs as much as possible; i.e. `yarn test path/to/example.test.ts`.

Run `yarn validate` from the workspace root for final validations — it typechecks, lints, tests, and builds all packages. The script merges stderr into stdout, prints `Exit code: N` at the end, and writes everything to `./yarn-validate-output.log`. **Run only `yarn validate` — do not add `2>&1`, `echo $?`, or any other wrapper.** Exit code 0 means all checks passed.
</validation>
