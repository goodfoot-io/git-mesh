# Understanding hook output

A one-pager for contributors and operators wiring the advice subsystem into a Claude Code session. The reader to keep in mind is a developer who has installed the hooks, sees text appearing in `additionalContext` and in the session transcript, and wants to know what each surfacing means and when to expect it. The hooks are a thin delivery layer — what they inject is the rendered output of the `git mesh advice` CLI for the current session — but the *timing* and *trigger* of each invocation is what shapes the experience around an assistant.

## What the hooks inject

The hooks do not invent advice. Each one composes a session-scoped read of the workspace's standing state by calling a `git mesh advice <session-id> <verb>` subcommand, and routes the resulting plain text into two surfaces:

- **`additionalContext`** — material the assistant sees on its next turn. This is where routing belongs: the other anchors in the mesh the assistant should also consider, the why that defines the subsystem the anchors form, the mesh name that labels the coupling.
- **`systemMessage`** — the same text mirrored into the transcript, so the developer reading the conversation later sees exactly what the assistant saw. The two surfaces always carry identical bytes; there is no agent-only channel and no developer-only channel.

If a render produces nothing, neither surface is written. Silence is a valid output. A turn with no injection is the steady state, not a failure.

The hook layer fails closed: if the workspace is not a git repository, if the hook payload is missing a session id, if the resolved repo cannot be located, or if the underlying `git mesh advice` call returns nothing or non-zero, the hook exits 0 and writes nothing rather than surfacing an error.

## When each hook fires

Three events drive the delivery layer (`plugins/git-mesh/hooks/hooks.json`). Only one of them produces user-visible output; the other two manage the per-session state the renderer reads from.

### PreToolUse — capture a snapshot pair

Matcher: `Edit|Write|MultiEdit|Bash|mcp__.*`. Script: `bin/advice-pre-tool-use.sh`.

For tools whose effects on the workspace are not visible from their structured input — `Bash` and any `mcp__*` tool — the hook calls `git mesh advice <sid> mark <tool_use_id>` to capture a before snapshot of the working tree. PostToolUse then captures the after snapshot and diffs the pair, so file changes produced as side effects of a shell command or MCP call can be attributed back to the exact tool call that caused them.

For tools the matcher includes but the script's deny-list short-circuits (`Edit`, `Write`, `MultiEdit`, plus the read-only set `Read`/`Grep`/`Glob`/`LS`/`WebFetch`/`WebSearch` if they reach this script), no snapshot is taken — PostToolUse already has structured input that names the affected path and range directly.

PreToolUse never injects text. Its only job is to leave a snapshot pair on disk so PostToolUse has something to diff.

### PostToolUse — render advice for what just happened

Matcher: `Read|Edit|Write|MultiEdit|Bash|mcp__.*`. Script: `bin/advice-post-tool-use.sh`. **The only injection point.**

Per tool, it picks the right `git mesh advice` verb:

- **`Read`** → `git mesh advice <sid> read <path>[#L<offset>-L<end>] [<tool_use_id>]`. The session records the read; if the read intersects an anchor in any mesh, the renderer surfaces the rest of the mesh (other anchors, the why) so the assistant sees what the read just touched.
- **`Edit` / `MultiEdit`** → For each hunk in the structured patch, `git mesh advice <sid> touch <tuid> <path>#L<new_start>-L<new_end> modified`. If a hunk has `newLines == 0` (whole-file deletion), or no structured patch is present, falls back to a whole-file `touch <tuid> <path> modified` once.
- **`Write`** → `git mesh advice <sid> touch <tuid> <path> {added|modified}` (`added` when the response reports `type=create`, `modified` otherwise).
- **`Bash`, `mcp__*`, anything else** → `git mesh advice <sid> flush <tuid>`. This is where the PreToolUse snapshot pair pays off: `flush` diffs the before/after snapshots and routes advice for any anchor the side effects crossed.

The output of the chosen verb is JSON-wrapped into `{hookSpecificOutput: {hookEventName: PostToolUse, additionalContext: <text>}, systemMessage: <text>}`. Empty output writes nothing.

### SessionEnd — clean up session state

Matcher: `*`. Script: `bin/advice-session-end.sh`.

Calls `git mesh advice <sid> end`, which removes the per-session advice directory and any leftover snapshot pairs left under `.git/mesh/advice/<sid>/`. SessionEnd does not inject text; it exists so the on-disk session store does not grow without bound across many sessions.

There is no `SessionStart` hook in the current plugin. The session store is created lazily by the first `git mesh advice <sid> <verb>` call from PostToolUse.

There is no `Stop` hook. Catching deferred side effects from the assistant's full turn is folded into PostToolUse's `flush` step on the last tool call.

There is no `UserPromptSubmit` hook. Path-shaped tokens lifted from prompts are not currently fed to the advice render.

## What the injected text looks like

Every injection is plain text rendered by `git mesh advice` (`packages/git-mesh/src/advice/render.rs`). The shape is consistent across verbs.

A finding for a single mesh:

```
src/checkout.tsx#L88-L120 is in the billing/checkout-request-flow mesh with:
- api/charge.ts#L30-L76 (CHANGED)
- docs/billing/charge-contract.md
```

- **Header line**: `<active-anchor> is in the <mesh> mesh with:` when a triggering anchor is known. Otherwise `<mesh> mesh contains:`. The active anchor is whatever the developer or the assistant just touched (the path the read landed on, the hunk that was edited, the partner that drifted under a side effect).
- **Bullets**: each related anchor in the mesh, in `<path>#L<start>-L<end>` form (or bare path for whole-file anchors). A status clause in parentheses follows the address when the anchor is anything other than `FRESH`. Markers used here are `(CHANGED)`, `(MOVED)`, `(ORPHANED)`, `(CONFLICT)`, `(SUBMODULE)`, `(DELETED)`, `(RENAMED)`. Absence of a clause means `FRESH`. (These are the rendered forms — internally the markers are `[CHANGED]` etc.; `format_status` strips the brackets and wraps in parentheses for the user-facing line.)
- **Excerpt block** (optional, density ≥ 1): a few lines of the related anchor's bytes, with its address, when the anchor is excerptible. Skipped for whole-file anchors and for the non-excerpt markers `[ORPHANED]`, `[CONFLICT]`, `[SUBMODULE]`, `[DELETED]`.
- **Command block** (optional, density 2): a one-line lead-in and an indented `git mesh …` command when the next step is unambiguous (re-record after edits, follow a rename, narrow or retire a mesh, record a candidate mesh).

A new-mesh suggestion (cross-cutting candidate the suggest pipeline scored High or High+):

```
Possible implicit semantic dependency between:
  - web/checkout.tsx#L88-L120
  - api/charge.ts#L30-L76

Record the link if real:
  git mesh add <name> \
    web/checkout.tsx \
    api/charge.ts
  git mesh why <name> -m "What these anchors do together."
```

Multiple stanzas in one injection are separated by `\n---\n\n`.

## What the hooks deliberately do not inject

- **Acknowledgements the developer or the assistant just received from a write command.** Advice composes on top of the rest of the CLI; the hooks never restate "updated `<ref>`" or "renamed `<old>` to `<new>`".
- **Findings the session has already been told about.** Once a relationship has been surfaced for a specific reason in this session, the renderer's "advice-seen" set suppresses it until the situation changes. Each injection reports what is new since the previous render, not the full standing state.
- **Anything when no heuristic clears its bar.** Only suggestions banded `High` or `High+` reach a render; a heuristic without the inputs it needs to be confident stays silent.
- **Editor or agent-specific shapes.** No LSP payloads, no JSON schemas tuned to a particular tool, no network calls. The same bytes appear in `additionalContext`, in `systemMessage`, and on the developer's terminal at the prompt.

## When something looks wrong

- **An injection appeared and the developer thinks it shouldn't have.** The render is reporting what is new since the last render in this session. If the same finding seems to repeat, the underlying state changed enough to clear the suppression filter — that is the routing working, not noise. If it genuinely repeats unchanged, that is a bug in the suppression layer (`advice_seen_set`) and should be filed against the render, not the hooks.
- **No injection appeared when the developer expected one.** Most common causes: the suggestion banded below `High`; the relationship has already been surfaced once in this session; the workspace is not a git repository; the snapshot pair PreToolUse should have written for a `Bash`/MCP call did not survive (check whether `GIT_MESH_ADVICE_DEBUG=1` and re-run — debug mode mirrors stderr from the `git mesh advice` calls into `systemMessage`); the session store was wiped by an earlier `SessionEnd` cleanup before this turn.
- **The same text appeared in `additionalContext` and in the transcript.** That is by design — the two surfaces always carry identical bytes so the developer reading the transcript later sees exactly what the assistant saw on its next turn.
- **A hook crashed.** All scripts trap errors and exit 0 with a `git-mesh advice hook: error rc=<n> at line <l>` breadcrumb on stderr; an internal failure must never block a turn or surface as a non-blocking exit-code error in the transcript.
