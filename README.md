# git-mesh

`git-mesh` is a Rust CLI for recording durable relationships between exact
anchors in a Git repository. It stores mesh metadata in Git refs so teams
can review, fetch, push, and audit those relationships alongside the code they
describe.

The monorepo ships:

- **`@goodfoot/git-mesh`** - the Rust CLI and npm wrapper
- **`goodfoot.git-mesh`** - a lightweight VS Code extension that manages the
  packaged `git-mesh` binary and exposes command entry points

The extension intentionally does not include a visualization webview yet. The
current goal is reliable binary resolution and command execution; richer mesh
visualization will be added later.

## CLI

During local development, run commands from `packages/git-mesh`:

```bash
cd packages/git-mesh
yarn build
```

Common command shape:

```bash
git mesh doctor
git mesh add checkout-request-flow src/client.ts#L10-L40 src/server.ts#L20-L64
git mesh why checkout-request-flow -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git mesh commit checkout-request-flow
git mesh stale checkout-request-flow
```

See [docs/git-mesh-the-missing-handbook.md](./docs/git-mesh-the-missing-handbook.md)
for the project model and workflow.

### Exit codes

`git-mesh` follows the POSIX convention used by `git` and `cargo`:

- **0** — success.
- **1** — operational failure: the command was well-formed, but
  the environment or repository state prevents completion.
  Example: `git mesh fetch nope` when `nope` is not a configured
  remote, or `git mesh commit foo` with nothing staged.
- **2** — usage error: the command itself is malformed (unknown
  flag, missing required argument). Example: `git mesh fetch --bogus`.

`git mesh stale` overlays its own §10.4 contract on top of this:
exit 1 when drift is found, exit 0 with `--no-exit-code`. The
`pre-commit` subcommand likewise exits 1 on in-flight drift.

### `git mesh show --format` placeholders

| Placeholder | Expansion | Trigger |
|---|---|---|
| `%H` | Full mesh commit SHA | per commit |
| `%h` | Abbreviated mesh commit SHA (7 chars) | per commit |
| `%an` | Author name | per commit |
| `%ae` | Author email | per commit |
| `%ad` | Author date (RFC 2822) | per commit |
| `%ar` | Author date, relative | per commit |
| `%s` | Subject (first line of message) | per commit |
| `%p` | Anchor path | per anchor |
| `%r` | Anchor extent (`#L<s>-L<e>`, empty for whole-file) | per anchor |
| `%P` | Path + extent (`path#L<s>-L<e>`, or just path for whole-file) | per anchor |
| `%a` | Anchor SHA (full 40 chars) | per anchor |
| `%A` | Anchor SHA (8-char abbrev; full with `--no-abbrev`) | per anchor |
| `%%` | Literal `%` | — |
| `%n` | Newline | — |

When any per-anchor placeholder is present in the format string, one output line is emitted per anchor. Otherwise, one line is emitted per mesh commit. Unknown placeholders are rejected with exit code 2.

## VS Code Extension

The VS Code extension is named `git-mesh` and publishes as
`goodfoot.git-mesh`. For now it is a lightweight command and binary manager:

- resolves the packaged `git-mesh` executable for the current platform
- installs or retries the managed binary when needed
- exposes Git Mesh command entry points inside VS Code
- keeps terminal PATH integration focused on the managed binary

It does not register a custom editor, Markdown renderer, search UI, or webview.

## Monorepo Layout

```text
.
├── packages/
│   ├── git-mesh/       # @goodfoot/git-mesh Rust CLI
│   └── extension/      # goodfoot.git-mesh VS Code extension
├── npm/
│   └── git-mesh-*/     # platform-specific binary distribution packages
├── docs/
│   ├── git-mesh-the-missing-handbook.md
│   └── cross-compilation.md
└── scripts/
    ├── sync-versions.sh
    ├── validate.sh
    └── release.sh
```

## Development Prerequisites

Before running tests or building the CLI locally, install the following tools:

**cargo-nextest** (test runner):

```bash
cargo install cargo-nextest --locked
```

For faster installation using prebuilt binaries, see the
[nextest install docs](https://nexte.st/book/pre-built-binaries.html).

**Linker (Linux only):**

```bash
sudo apt-get install mold clang   # Ubuntu/Debian
```

On macOS no extra install is required — the mold linker config is gated to
Linux GNU targets only.

**Shared Cargo target directory:**

`packages/git-mesh` scripts default `CARGO_TARGET_DIR` to
`$HOME/.cache/git-mesh/cargo-target/<kind>`, shared across all worktrees and
`/workspace`. Override the root via `GIT_MESH_CARGO_TARGET_ROOT`:

```bash
GIT_MESH_CARGO_TARGET_ROOT=/tmp/my-target yarn test
```

Note: `yarn build:clean` (and any `cargo clean`) wipes the entire shared cache
at `$GIT_MESH_CARGO_TARGET_ROOT` (or `$HOME/.cache/git-mesh/cargo-target`),
affecting all worktrees that use that root.

## Contributing

```bash
git clone https://github.com/goodfoot-io/git-mesh.git
cd git-mesh
yarn install
yarn build
yarn validate
```

Use Yarn for all JavaScript package management. Per-package validation should
run from the package directory that contains the changed files:

```bash
cd packages/git-mesh
yarn lint
yarn typecheck
yarn test

cd ../extension
yarn lint
yarn typecheck
yarn test
```

Run `yarn validate` from the workspace root before finalizing code or
configuration changes.

### Manpage

The CLI ships a `git-mesh(1)` manpage generated from the clap definitions. To
browse it without installing:

```bash
MANPATH=packages/git-mesh/man man git-mesh
```

Regenerate after changing CLI flags or descriptions:

```bash
cd packages/git-mesh
yarn build:man
```

The regression test in `tests/manpage.rs` fails if the checked-in artifact
drifts from what the generator produces.

## Releases

Releases are tag-driven from `goodfoot-io/git-mesh`. CLI and extension release
assets use `git-mesh` names, including `git-mesh-v*` tags and
`git-mesh-cli-checksums.json`.

## License

MIT - Copyright (c) 2026 Goodfoot Media LLC. See [LICENSE](./LICENSE).
