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

<bash-tool-env-var-bug>
A bug prevents env var expansion when followed by a "|" character pipe.

This fails:

```bash
echo $HOME | cat # returns ''
```

Do this instead:

```bash
echo $(printenv HOME) | cat # returns '/home/user'

# or no pipe:

echo $HOME # returns '/home/user'

# or with a redirect:

echo $HOME 2>&1 # returns '/home/user'
```
</bash-tool-env-var-bug>

<validation>
Run validation from the package directory containing the changed files, using that package's scripts from `package.json` (e.g., `yarn lint`, `yarn typecheck`, `yarn test`).

Always focus test runs as much as possible; i.e. `yarn test path/to/example.test.ts`.

Run `yarn validate` from the workspace root for final validations — it typechecks, lints, tests, and builds all packages. The script merges stderr into stdout, prints `Exit code: N` at the end, and writes everything to `./yarn-validate-output.log`. **Run only `yarn validate` — do not add `2>&1`, `echo $?`, or any other wrapper.** Exit code 0 means all checks passed.
</validation>

<git-mesh>
A mesh names an implicit semantic dependency between two or more anchors - a **line-range anchor** (`path#L<start>-L<end>`) or a **whole-file anchor** — in code or prose — that is real, load-bearing, and not enforced by any type, schema, or test. It exists so a future developer touching one anchor learns, at that moment, what the other anchor relies on them to keep true. The standing question whenever you edit, read, or review is: does this region create or rely on a coupling that isn't visible from the lines themselves? If yes, and if you can name a concrete wrong decision someone would make under deadline when the related anchor changes silently, write a mesh.

```bash
# Stage the mesh: slug names the relationship, whole-file anchors the prose spec, line range anchors the exact parser bytes.
git mesh add billing/charge-request-contract \
  docs/api/charge.md \
  services/api/charge.ts#L30-L76

# One prose sentence in role-words; names which side is normative. Every new mesh needs a why before it can be committed.
git mesh why billing/charge-request-contract \
  -m "The published request spec states the body shape the parser honors; the spec is the source of truth when they disagree."

# The post-commit hook runs `git mesh commit`, anchoring the staged ranges to the commit that just landed.
git commit -m "Document charge request contract and wire the parser"
```
</git-mesh>