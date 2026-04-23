# AGENTS.md

## Golden Rule

After making any changes to code or configuration files, lint, type check, and run all tests. (Not required for markdown, JSON, or CSS changes.) All warnings and failures are blocking — do not dismiss any as "pre-existing" or "unrelated." A test that does not run because of an infrastructure error is a blocking condition.

## Workspace

- Yarn 4.x monorepo with packages in `./packages/` containing a Rust CLI (`packages/git-mesh`) and a VS Code extension (`packages/extension`).
- Do not use npm. Use `yarn` for all package management.
- Use local rather than origin branches.

## Conventions

- Greenfield implementation. No migrations, backwards compatibility, or fallbacks.
- Always choose the "right way" over the "easy way".
- Prefer "fail closed" workflows over "fail open" workflows.
- Do not include "Co-Authored-By" messages in commits.

## Documentation

Project documentation lives in `README.md` and `docs/`. Keep docs focused on `git-mesh`, the Rust CLI in `packages/git-mesh`, and the lightweight VS Code extension in `packages/extension`.

## Validation

Run validation from the package directory containing the changed files (e.g., `yarn lint`, `yarn typecheck`, `yarn test`). Always focus test runs: `yarn test path/to/example.test.ts`.

Run `yarn validate` from the workspace root for final validation. Exit code 0 means all checks passed.

## Tools

- `jsdoczoom` for TypeScript file exploration (use `--search` with unescaped regex patterns).
- `ripgrep` (`rg`) for code search.
