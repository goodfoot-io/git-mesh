# `git mesh advice` — DX goals

A one-pager for contributors shaping the advice subsystem. The reader
to keep in mind is a developer who runs `git mesh advice` after every
edit or repo change and wants the output to earn its place in their
loop. These are the experience commitments the command answers to;
anything that violates one of them is a bug, regardless of how clever
the underlying signal is.

## What `advice` is

`advice` does not introduce a new mesh concept. It composes the
existing mesh reads — `stale`, `ls`, `why`, `git mesh <name>` — into
routing for a developer mid-edit. Where `stale` answers "do anchored
bytes still match reality?", `advice` answers "given what just moved,
which mesh ranges deserve my attention right now, and what does each
mesh's why say about why I should care?" Every line of advice output
is something the developer could have reconstructed by hand from those
reads; the value is timing, ordering, and suppression, not new data.

## 1. The news is the partner, never the developer's own action

The developer already knows what they just edited. Advice exists to
point them at the *related* anchor — code or prose — on the other
side of an implicit semantic dependency. Their own change appears
only as the minimum context that makes the related side legible —
never as the headline.

## 2. Assume zero mesh knowledge

A reader who has never heard of `git mesh` should still understand the
default output. The mesh name, the one-sentence why describing the
relationship the ranges hold, and the ranges themselves should carry
the signal on their own. Anything beyond that appears only when the
basics cannot speak for themselves.

## 3. Surface consequences, not prescriptions

Advice describes what is now true across the repo: a range is
`CHANGED`, two files keep moving together, a reference still points at
an old name. It does not scold or guess intent. A concrete next step
is offered only when the action is unambiguous. The canonical example
is suggesting a mesh definition be extended — a `git mesh add ...`
range pair, optionally a `git mesh why ...` — once a co-change
pattern crosses the confidence threshold. Other allowed steps:
mechanical rewrites with a uniquely determined target, a specific
`git mesh` read command with concrete arguments, a read operation
likely to touch the current change set, and write operations the
system has high confidence in. When confidence drops, the step
degrades to a copyable `path#Lstart-Lend` locator rather than
disappearing.

## 4. Drift is the feature, not a warning

A relationship exists so that movement on one side routes attention to
the other. Output reads as routing, not as alarm. No severity, no red
text, no "warning:" prefix — state is conveyed factually, without
affect.

## 5. Detail scales with certainty

A glancing touch earns a one-line pointer. A change that crosses a
relationship earns enough context to compare the two sides. A
high-confidence signal — a stale reference, a structural conflict, a
candidate worth recording — earns the most context and a concrete
next step. Certainty earns ink; the ladder never runs in reverse.

## 6. Teach just before — or alongside — recommending

Contextual documentation arrives with, or slightly ahead of, the
recommendation it supports. When a situation is starting to look like
a candidate for an action, the explanation of that action can land
first, so that by the time the recommendation is concrete the
developer already knows what it means. A recommendation should never
be the developer's first encounter with the concept behind it.

## 7. Don't repeat what the developer has already been told

Once a relationship has been surfaced for a specific reason, advice
stays quiet about it until the situation changes. Each invocation
reports what is new since the previous `git mesh advice` run, not the
full standing state.

## 8. Don't echo what `git mesh` write commands already print

Advice composes on top of the rest of the CLI; it never restates an
acknowledgement the developer just received from a `git mesh` write
command (`add`, `rm`, `why`, `commit`, `mv`, `delete`, `revert`,
`restore`, `config`, `push`, `fetch`). Read commands (`stale`, `ls`,
`why` print, `git mesh <name>`) are the substrate advice is built on
— surfacing their data contextually is the point, not duplication.

## 9. Fail closed

When a heuristic lacks the inputs it needs to be confident, it stays
silent. A missed signal is cheaper than a wrong one. Advice that
cries wolf gets muted, and a muted channel cannot route attention at
all.

When no heuristic clears its bar, advice prints nothing and exits
zero. Silence is the answer. Explanatory prose for a concept appears
only when a finding escalates toward a recommendation, and only on
the first such appearance within a session; subsequent surfacings of
the same concept rely on the mesh name and the mesh's own why. A
mesh's why is one prose sentence naming the relationship the ranges
hold in role-words ("the doc," "the parser," "the runbook") — no
filenames the ranges already carry, no leading keywords like
`contract:` or `spec:`, no scolding — short, stable across
implementation churn at either anchor, and load-bearing; advice
prints it verbatim with every appearance of that mesh and is not
subject to Goal #7.

## 10. Generic by construction

Output is plain text a developer can read in any terminal, log, or
diff view. No editor APIs, no agent-specific shapes, no network
calls. The same output that helps a person at the prompt is what any
automation around the command consumes.

## Non-goals

- **IDE- or agent-specific output shapes.** No LSP payloads, no JSON
  schemas tuned to a particular tool. Output is plain text (Goal #10).
- **Multi-session coordination.** Advice does not mediate concurrent
  developers editing the same working tree.
- **Network calls.** No telemetry, no remote lookups. Everything
  comes from the local repo and its meshes; `git mesh fetch` is the
  developer's call.
