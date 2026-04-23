# Plan: rename `git mesh message` to `git mesh why`

## Goal

Rename the mesh-message subcommand and its vocabulary throughout the
tool so users (and LLMs) are pushed toward the correct frame: the
durable text attached to a mesh describes **why the relationship
exists**, not what the latest change was.

The name `message` imports the `git commit -m` mental model, which is
the exact analogy that led a recent handbook example to suggest
"Re-anchor route after session helper extraction" as a mesh message —
a commit-log entry, not a relationship description. Renaming the
verb shifts the frame at the point of use: `git mesh why <name> -m "…"`
reads as an answer to a question, not as a log entry.

This plan is a follow-up to `docs/stale-layers-plan.md` and should land
after that refactor is stable. The two are independent but share
several touch points (CLI parsing, handbook §"Daily workflow",
command reference), so landing them in order avoids conflicts.

## Precedent

- `yarn why <pkg>` and `npm why <pkg>` — established question-form
  subcommands in widely used CLIs. The pattern is not novel.
- ADR (Architecture Decision Records) — "rationale" is the canonical
  section header for *why this decision exists*. A mesh is a micro-ADR:
  "these ranges belong together because…"

## Non-goals

- No change to the underlying git-commit storage. A mesh commit's
  message is still a git commit message by git's own definition; the
  rename is user-facing vocabulary, not git-layer vocabulary.
- No deprecation alias. Greenfield per `<greenfield>` in `CLAUDE.md` —
  `git mesh message` is replaced, not dual-supported.
- No rename of internal Rust field names that happen to be called
  `message` *as a pass-through to the git commit message field* — those
  stay as they are to match git vocabulary. See "Internal naming"
  below for the distinction.

## Behavior

### B1 — Writer form

```bash
git mesh why <name> -m "<text>"
git mesh why <name> -F <file>
git mesh why <name> --edit
```

Flags are unchanged from today's `git mesh message`. Behavior:

- `-m <text>` sets the staged why text inline.
- `-F <file>` reads it from a file.
- `--edit` (or bare `git mesh why <name>`) opens `$EDITOR` on a
  pre-populated template.
- The staged why is consumed at `git mesh commit` time and becomes the
  resulting mesh commit's git commit message.

### B2 — Reader form

```bash
git mesh why <name>                 # prints the current mesh's why text
git mesh why <name> --at <commit>   # prints the why at a historical state
```

Bare `git mesh why <name>` with no writer flags prints the current
mesh's why text. This is new capability — today's `git mesh message`
has no reader form; users read the message via `git mesh <name>`.
Adding a dedicated reader matches `yarn why`'s read-first ergonomics.

Ambiguity resolution: if `<name>` is followed by no further arguments,
it's a read. If any of `-m`/`-F`/`--edit` is present, it's a write.
These are mutually exclusive modes.

### B3 — Inheritance is unchanged

At `git mesh commit` time, if no why is staged, the parent mesh
commit's message is inherited. This is today's behavior
(`mesh/commit.rs` L124-L128) and the core reason the handbook's recent
clarification is true: routine re-anchors don't rewrite the why.

### B4 — Error when a new mesh has no why

A new mesh has no parent to inherit from. Committing it without a
staged why fails with `Error::WhyRequired(mesh_name)` (renamed from
`Error::MessageRequired`). Error message text points the user at
`git mesh why <name> -m "…"`.

## Internal naming

| Surface | Today | After |
|---|---|---|
| CLI subcommand | `git mesh message` | `git mesh why` |
| CLI flag names | `-m`, `-F`, `--edit` | unchanged |
| Staging file suffix | `.msg` | `.why` |
| Error variant | `Error::MessageRequired` | `Error::WhyRequired` |
| Rust field on mesh-layer structs representing the staged text | `message: Option<String>` | `why: Option<String>` |
| Rust field on git-layer structs that pass through to `gix` commit message | `message` | `message` (unchanged — git vocabulary) |

Rule of thumb: if the code is manipulating the mesh-layer concept
("the text the user attached to this mesh"), call it `why`. If the
code is handing bytes to `gix::commit` as the git commit message, call
it `message`. The boundary is where we hand off to git.

## Reserved names

`why` joins the reserved mesh-name list; `message` leaves it. Update
the list in `docs/git-mesh-the-missing-handbook.md` and the CLI's
validation table.

## Implementation phases

Single shipping unit. This is a rename, not a redesign; there's no
meaningful intermediate state worth phasing.

### Phase 1 — Rename

**CLI.**
- Rename the `message` subcommand to `why` in `cli/mod.rs` and wherever
  clap subcommands are declared.
- Add the reader form: bare `git mesh why <name>` (no writer flags)
  prints the current why text resolved at HEAD (or `--at <commit>`).
- Update the reserved-name validator.

**Storage.**
- Staging: `.git/mesh/staging/<name>.msg` → `.git/mesh/staging/<name>.why`.
- Any on-disk migration is unnecessary per `<greenfield>` — tests and
  fixtures are regenerated.

**Types and errors.**
- Mesh-layer staged-text field rename across `staging.rs`,
  `mesh/commit.rs`, `types.rs`: `message: Option<String>` → `why: Option<String>`.
- `Error::MessageRequired` → `Error::WhyRequired`. Error message text
  updated to reference the new subcommand.

**Tests.**
- Global rename in `packages/git-mesh/tests/` of `mesh message` →
  `mesh why`, and of any staging-file path assertions using `.msg`.
- Add a new reader-form integration test: `git mesh why <name>`
  prints the current why; `git mesh why <name> --at <commit>` prints a
  historical why.
- Add an error-form test: committing a new mesh without a staged why
  produces `Error::WhyRequired` with guidance pointing at
  `git mesh why <name> -m`.

**Docs.**
- `docs/git-mesh-the-missing-handbook.md`:
  - Global replace of `git mesh message` → `git mesh why`.
  - Update §"Changing only the message" heading to "Changing the
    relationship description" and rewrite the paragraph accordingly.
  - Update §"Write useful mesh messages" to "Write a useful why"
    (or similar) and keep the existing guidance (already correct).
  - Update §"Command reference" (writer + new reader form).
  - Update §"Best-practice checklist" and the reserved-names sentence.
  - Update the pre-commit / post-commit workflow narration where it
    mentions staging a "message."
- `README.md`: global replace where `git mesh message` appears.
- `docs/stale-layers-plan.md`: update any example lines that use
  `git mesh message` (the plan is a future-looking design doc; keeping
  it vocabulary-aligned avoids reintroducing the old frame).

### Acceptance

- `yarn validate` green per `<golden-rule>` in `CLAUDE.md`.
- `git mesh --help` lists `why`, not `message`.
- `git mesh why <name>` with no flags prints the current why.
- `git mesh message <name> …` returns a clap "unknown subcommand" error
  — no silent alias.
- Grep: no remaining `git mesh message` in `docs/`, `README.md`, or
  `packages/git-mesh/src/`, except inside historical examples that
  deliberately reference old behavior (flag them if found).

## Risks and mitigations

- **User muscle memory.** People who type `git mesh message` will get
  an error. Acceptable: the failure is immediate and the error text
  names the new subcommand, so the cost is one typo per user. No
  deprecation alias — greenfield.
- **Scripts in the wild.** Any external automation calling
  `git mesh message` breaks. If this tool has a published user base
  at rename time, the release notes must call this out prominently;
  otherwise the noise is minimal.
- **Shell completion.** If completion scripts exist, they need
  regenerating. Add to the Phase 1 acceptance checklist.
- **Handbook drift.** Mesh-message vocabulary will reappear in future
  docs if authors default to the `git commit -m` frame. Mitigate by
  adding a short note in `CLAUDE.md` pointing contributors at the
  "mesh why is relationship, not changelog" rule when writing examples.

## Out-of-scope follow-ups

- Renaming the public-facing term "message" to "why" in git-commit
  subject lines that git-mesh itself constructs for automated mesh
  commits (none today). If we ever add automated message composition,
  it would use "why"-vocabulary.
- A `git mesh why --format=json` shape beyond plaintext. Not needed
  today.
