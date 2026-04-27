# Understanding hook output

A one-pager for contributors and operators wiring the advice subsystem
into a Claude Code session. The reader to keep in mind is a developer
who has installed the hooks, sees text appearing in
`additionalContext` and in the session transcript, and wants to know
what each surfacing means and when to expect it. The hooks are a thin
delivery layer — what they inject is the same routing the developer
would see at the prompt — but the *timing* and *trigger* of each
injection is what shapes the experience around an assistant. These
are the commitments the hooks answer to; anything that violates one
of them is a bug in the delivery layer, regardless of what the
underlying signal looked like.

## What the hooks inject

The hooks do not invent advice. Each one composes a session-scoped
read of the workspace's standing state — the same read a developer
could run by hand — and routes the resulting plain text into two
surfaces:

- **`additionalContext`** — material the assistant sees on its next
  turn. This is where routing belongs: the partner the assistant
  should also consider, the why that names the relationship the
  ranges hold, the mesh name that labels the coupling.
- **`systemMessage`** — the same text mirrored into the transcript,
  so the developer reading the conversation later sees exactly what
  the assistant saw. The two surfaces always carry identical bytes;
  there is no agent-only channel and no developer-only channel.

If a render produces nothing, neither surface is written. Silence is
a valid output. A turn with no injection is the steady state, not a
failure.

## When each hook fires

Four events drive the delivery layer. Each one has a single,
distinct job; together they cover the moments when the workspace's
relationships could have shifted under the developer or the
assistant.

### 1. Session start — establish the baseline

Every session, including the fresh session id created by a compact,
captures the current workspace as the baseline that later renders
diff against. Nothing is injected. The job is to make sure later
hooks have something to compare against; a session without a
baseline cannot route attention because it cannot tell what moved.

### 2. User prompt submit — read what the developer named

When the developer submits a prompt, path-shaped tokens lifted from
the prompt are recorded as reads. Then a render runs and, if it has
something to say, lands in `additionalContext` and `systemMessage`
before the assistant's turn begins. The framing matches Goal #1 of
the advice DX: the news is the partner, never the developer's own
typing. A path the developer mentioned appears only as the locator
that makes the related side legible.

### 3. Post tool use — read what the assistant just touched

After the assistant finishes a tool call that may have moved the
workspace — a file read, a file write, a notebook edit, a shell
command, an MCP call — the hook resolves the repository the tool
actually operated in (file path for read/edit tools, parsed `cd` and
`git -C` targets for shell, the session cwd as a fallback for MCP
tools without a uniform target) and renders advice there. If the
tool touched multiple repositories, each one renders independently
and the results combine into a single injection. This is where most
real-time routing happens: the moment after the assistant changed
something is the moment the developer most needs to know what else
the change crossed.

### 4. Stop — catch anything the per-tool renders missed

When the assistant finishes its turn, a final render runs to surface
anything the per-tool hooks did not catch — typically staging or
configuration changes a shell command produced as a side effect.
Stop is informational only and never blocks turn end. Renders
triggered by `max_tokens` or `stop_sequence` are skipped, because
those reasons mean the turn ended without intent and routing
attention there would be noise.

## What the injected text looks like

The body of every injection is plain text the developer could read
in any terminal, log, or diff view (Goal #10 of advice DX). It
follows the same shape regardless of which hook produced it:

- A header naming the mesh and the one-sentence why describing the
  relationship the ranges hold, so a reader who has never heard of
  the underlying tooling still understands the coupling (Goal #2).
- A trigger locator pointing at what routed attention — the path
  the developer named, or the range the assistant just touched —
  rendered as the minimum context that makes the related side
  legible (Goal #1).
- The partner addresses on the other side of the relationship,
  carrying the headline. State is conveyed factually — `CHANGED`,
  `MOVED`, address-only when the partner is not excerptible — with
  no severity, no red text, no "warning:" prefix (Goal #4).
- Optionally, a concrete next step when the action is unambiguous
  and a one-time explanation block when a finding escalates toward a
  recommendation for the first time in the session (Goals #3, #5,
  #6).

Detail scales with certainty. A glancing touch earns a one-line
pointer. A change that crosses a relationship earns enough context
to compare the two sides. A high-confidence signal — a stale
reference, a structural conflict, a candidate worth recording —
earns the most context and a concrete next step.

## What the hooks deliberately do not inject

- **Acknowledgements the developer or the assistant just received
  from a write command.** Advice composes on top of the rest of the
  CLI; the hooks never restate "updated `<ref>`" or "renamed `<old>`
  to `<new>`" (Goal #8).
- **Findings the session has already been told about.** Once a
  relationship has been surfaced for a specific reason in this
  session, the hooks stay quiet about it until the situation
  changes. Each injection reports what is new since the previous
  render, not the full standing state (Goal #7).
- **Anything when no heuristic clears its bar.** A heuristic without
  the inputs it needs to be confident stays silent. A missed signal
  is cheaper than a wrong one (Goal #9). The hooks fail closed: if
  the workspace is not a repository, if the session has no id, if
  the render returns nothing, the hook exits zero and writes
  nothing.
- **Editor or agent-specific shapes.** No LSP payloads, no JSON
  schemas tuned to a particular tool, no network calls. The same
  bytes appear in `additionalContext`, in `systemMessage`, and on
  the developer's terminal at the prompt (Goal #10).

## When something looks wrong

Three diagnostics cover most surprises:

- **An injection appeared and the developer thinks it shouldn't
  have.** The render is reporting what is new since the last render
  in this session. If the same finding seems to repeat, the
  underlying state changed enough to clear the suppression filter —
  that is the routing working, not noise. If it genuinely repeats
  unchanged, that is a bug in the suppression layer and should be
  filed against the render, not the hooks.
- **No injection appeared when the developer expected one.** The
  most common cause is that the heuristic for that reason kind did
  not clear its bar (Goal #9), or the relationship has already been
  surfaced once in this session (Goal #7), or the workspace has no
  baseline because the session-start hook did not run (re-launching
  the session re-establishes it). Silence is the steady state; the
  bar to break it is intentionally high.
- **The same text appeared in `additionalContext` and in the
  transcript.** That is by design. The two surfaces carry identical
  bytes so the developer reading the transcript later sees exactly
  what the assistant saw on its next turn.
