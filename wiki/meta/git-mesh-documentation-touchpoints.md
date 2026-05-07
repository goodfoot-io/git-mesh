---
title: Git Mesh Documentation Touchpoints
summary: Canonical map of git-mesh CLI implementation, operator instructions, and maintenance references that must stay aligned when git-mesh documentation behavior changes.
tags:
  - meta
  - git-mesh
  - tooling
---

This page is the maintenance map for future git-mesh documentation updates. When command behavior or recommended usage changes, update the implementation-facing source of truth first, then walk the operator-facing documents and automation references listed here so guidance does not drift.

For the broader rules governing wiki pages, see [[Wiki Organization]].

## Command Behavior Source Of Truth

The primary source of truth for top-level CLI behavior is the Clap configuration in [packages/git-mesh/src/cli/mod.rs](packages/git-mesh/src/cli/mod.rs#L28-L119). That block defines the `Cli` struct, the `Commands` enum, and the help text for every subcommand. The [dispatch function](packages/git-mesh/src/cli/mod.rs#L448-L500) in the same file routes parsed commands to their handlers.

The pre-classification logic that makes `git mesh <name>` route to `Commands::Show` rather than failing as an unknown subcommand lives in [packages/git-mesh/src/main.rs](packages/git-mesh/src/main.rs#L39-L111). That block is what resolves the ambiguity between a mesh name positional and a subcommand name before Clap parses the arguments. The reserved subcommand set it checks against is defined in [packages/git-mesh/src/validation.rs](packages/git-mesh/src/validation.rs#L7-L35).

When documentation changes are about subcommand behavior or exit-code semantics, the relevant handler implementations live under `packages/git-mesh/src/cli/` (e.g., `stale_output.rs`, `commit.rs`, `show.rs`).

## Operator-Facing Documentation

These files are the public guidance surfaces most likely to drift when the CLI contract changes:

- The [README CLI section](README.md#L18-L55) is the reader-facing quick reference for common command shapes, exit code semantics, and `--format` placeholders.
- The [repository `CLAUDE.md` git-mesh instructions](CLAUDE.md#L97-L113) shape how agents in this workspace are told to stage and commit meshes.
- The git-mesh handbook skill (bundled with the `git-mesh` plugin, not tracked in this repo) is the highest-leverage agent workflow contract for creating, updating, and querying meshes.
- The git-mesh implicit-semantic-dependencies skill (bundled with the `git-mesh` plugin, not tracked in this repo) guides agents through identifying and recording coupling that has no schema or test enforcement.
- The [man page](packages/git-mesh/man/git-mesh.1) is the installed reference for the CLI. It is generated from the Clap config; changes to command signatures surface here automatically on the next build, but prose descriptions require manual attention.

If a documentation update changes the recommended operator workflow, all of these surfaces should be checked explicitly, not only the page that first exposed the inconsistency.

## Update Order

When git-mesh CLI behavior or documentation changes, use this order:

1. Confirm the implementation in [packages/git-mesh/src/cli/mod.rs](packages/git-mesh/src/cli/mod.rs#L28-L119) (Clap config and Commands enum) and [packages/git-mesh/src/main.rs](packages/git-mesh/src/main.rs#L39-L111) (pre-classification and dispatch).
2. Update the primary user docs in [README.md](README.md#L18-L55) and [CLAUDE.md](CLAUDE.md#L97-L113).
3. Update the agent workflow contract in the git-mesh handbook skill (bundled with the `git-mesh` plugin, not tracked in this repo).
4. Update secondary references such as the implicit-semantic-dependencies skill and the man page.
5. Run `wiki check` on the touched pages so fragment links validate and the page participates in `wiki stale`.

## References

- [CLI mod](packages/git-mesh/src/cli/mod.rs#L1-L50)
- [main.rs dispatch](packages/git-mesh/src/main.rs#L39-L111)
- [validation.rs reserved names](packages/git-mesh/src/validation.rs#L7-L35)
