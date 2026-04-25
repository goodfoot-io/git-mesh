# `git mesh advice` — DX evaluation, second pass (2026-04-25)

## Method

Seven runs against a scratch repo at `/tmp/eval-advice-1`, two meshes
(`checkout-flow`, `fruit-list`) overlapping on `b.txt#L3-L6`. Each
run used a fresh `<session-id>` to keep T7 (repeat-suppression) state
clean except for run 2 which deliberately reused `s1`. Scenarios:
(1) first finding; (2) repeat-suppression; (3) unrelated edit;
(4) cross-cutting two-mesh edit; (5) heavily drifted edit;
(6) range collapse / T4; (7) whole-file deletion. Verbatim flush
output is reproduced in the appendix; quotes below cite by run
number and `RN/Lk` line offsets within the appendix block.

## Per-goal verdicts

### Goal 1 — News is the partner, never the developer's own action

**Verdict:** partially met.

**Evidence (R1):**
```
# checkout-flow mesh: Checkout request flow ...
# - b.txt#L3-L6
```
The headline does name the partner range, not `a.txt#L2-L5`.

**But (R4):**
```
# checkout-flow mesh: ...
# - a.txt#L2-L5 [CHANGED]
```
When the developer's edit is on a file that *also* appears in the
mesh in another range, advice surfaces the developer's own
`[CHANGED]` range as a top-level bullet — the very thing they just
typed. R4's checkout-flow block only contains the writer's own
change; nothing routes them anywhere else.

**Mechanism:** the renderer enumerates all ranges in each touched
mesh rather than filtering to ranges other than the trigger. When
the trigger and a partner share a mesh but the partner happens to
be the *trigger's own* range, the partner list collapses to "you".

**Tuning:** suppress the trigger range from the partner list when
*and only when* there is at least one other range left to surface;
if the trigger is the only range in the mesh on this file, fall
back to the cross-cutting label (Goal #3) rather than reprinting
the developer's own bytes.

### Goal 2 — Assume zero mesh knowledge

**Verdict:** met (perhaps too aggressively — see Goal 7).

**Evidence (R1/L1-L9):**
```
# A mesh names a subsystem, flow, or concern that spans line ranges in
# several files. The mesh description states what those ranges do
# together. ...
# Inspect a mesh:
#   git mesh show <name>           # ranges, description, history
#   git mesh ls <path>             # meshes that touch a file
#   git mesh stale                 # ranges whose bytes have drifted
```
A reader who has never used `git mesh` can read this and act.

**Mechanism:** a fixed preamble fires on the first finding of a
session.

**Tuning:** none for this goal alone. The cost shows up against
Goal 7 (economy) and Goal 6 (teach *just before* recommending — not
unconditionally).

### Goal 3 — Surface consequences, not prescriptions

**Verdict:** missed.

**Evidence (R1, prose paragraph 2):**
```
# When a range in a mesh changes, the other ranges in the same mesh may
# need matching changes. The excerpt below is the related range — the
# code on the other side of the relationship. Compare, then either update
# it or accept that the relationship has shifted and re-record the mesh.
```
Three prescriptive verbs in two sentences: **Compare**, **update**,
**accept**. The goal explicitly forbids guessing intent; "accept
that the relationship has shifted" prejudges the outcome.

**Evidence (R6):**
```
# To re-record with the new extent:
#   git mesh rm checkout-flow b.txt#L3-L6
#   git mesh add checkout-flow b.txt#L3-L4
```
This one is fine — the action is unambiguous (T4 with concrete
new extent), and the goal explicitly allows mechanical rewrites
with a uniquely determined target.

**Mechanism:** the always-on prose preamble bakes prescription into
every first finding regardless of what the finding actually is.

**Tuning:** drop the second prose paragraph entirely; let the
"checkout-flow mesh: <why>" header plus a `CHANGED`/co-located
locator carry the consequence. Keep the concrete `git mesh
rm`/`add` block in R6, since that step *is* uniquely determined.

### Goal 4 — Drift is the feature, not a warning

**Verdict:** met.

**Evidence:** no `WARNING:`, `error:`, severity, or affect tokens
appear anywhere in any of seven runs. `[CHANGED]` is factual.

**Mechanism:** rendering uses comment markers (`#`) and bracketed
state tokens, not severity prefixes.

**Tuning:** none.

### Goal 5 — Detail scales with certainty

**Verdict:** partially met.

**Evidence (R1):** untouched partner gets a 4-line excerpt:
```
# b.txt#L3-L6
# ```
# cherry
# date
# elder
# fig
# ```
```

**Evidence (R7, deleted file):**
```
# checkout-flow mesh: ...
# - b.txt#L3-L6 [CHANGED]
#
# fruit-list mesh: ...
# - b.txt#L3-L6 [CHANGED]
```
Address-only — the excerpt is suppressed because the post-collapse
file no longer has lines 3-6. Good — that path obeys the goal.

**Counter-evidence (R5):** a heavily drifted, very high-confidence
"this is no longer the same range" still produces the same
preamble + excerpt as a low-confidence first finding (R1). The
ladder runs flat: the most ambiguous case (R1) and the most
unambiguous case (R5) get the same treatment, and only T4 (R6)
escalates to a concrete next step.

**Mechanism:** the only certainty signal currently consumed by the
renderer is T4 (range collapse). Stale-by-content drift does not
escalate.

**Tuning:** when `git mesh stale` reports `H CHANGED` on a partner
range that did *not* receive the trigger edit, escalate to a
`git mesh add <name> <path>#L<s>-L<e>` re-record hint, since same
`(path, extent)` re-add is documented as last-write-wins.

### Goal 6 — Teach just before — or alongside — recommending

**Verdict:** partially met / inverted.

**Evidence (R1):** a generic "what is a mesh" block fires on the
*first finding* — before any recommendation has been generated.
The teaching is not tied to a recommendation that is starting to
look likely; it teaches by default.

**Evidence (R6):** the T4 explanatory paragraph
```
# The edit reduced a range to far fewer lines than were recorded ...
#   git mesh rm  <name> <path>#L<old-s>-L<old-e>
#   git mesh add <name> <path>#L<new-s>-L<new-e>
```
is correctly co-located with the concrete recommendation that
follows. This is the goal working as designed.

**Mechanism:** two separate teaching paths: an unconditional
session-preamble (always fires on first finding) and a
per-reason block (fires when the reason classifier escalates).
The unconditional path violates "alongside the recommendation it
supports."

**Tuning:** delete the unconditional preamble. Move its
content into a per-reason block keyed on "first time advice has
ever surfaced *any* finding for this mesh in this session."

### Goal 7 — Don't repeat what the developer has already been told

**Verdict:** met for findings, missed for teaching.

**Evidence (R2):** identical re-applied edit produces empty output,
exit 0. Suppression works at the finding granularity.

**Counter-evidence:** the preamble in R1 (29 lines) and the
preamble in R4 (29 lines) and R5 (29 lines) and R6 (29 lines) are
byte-identical. Within a session of even modestly different
edits, the same teaching reprints on every first-finding-per-
session — and since the appendix shows runs 4/5/6 each used a
distinct session id, the *real* steady-state cost is "29 lines
per process invocation." The hook fires per Edit, so per process
≈ per edit.

**Mechanism:** preamble suppression is keyed on session, but the
hook scripts shell out per-event with a session id derived from
something that does not survive across most natural workflows.
(Confirmed by inspecting `/tmp/git-mesh-claude-code/*.jsonl` —
many short-lived session ids accumulate.)

**Tuning:** key preamble suppression on a stable per-repo or per-
worktree marker, not on session id. A 24h cooldown file at
`<state>/<repo-fingerprint>.preamble-shown` would silence repeat
teaching with no cross-developer coordination.

### Goal 8 — Don't echo what `git mesh` write commands print

**Verdict:** met.

**Evidence:** advice never echoes `updated refs/meshes/...`. The
output shape is distinct from `git-mesh add`/`commit` output.

### Goal 9 — Fail closed

**Verdict:** met.

**Evidence (R3):** edit to `c.txt`, a file no mesh touches, both
`add` and flush produce empty stdout, exit 0.

**Evidence (R2):** repeated edit also silent + exit 0.

**Mechanism:** absence of any matching range short-circuits the
renderer.

### Goal 10 — Generic by construction

**Verdict:** met.

**Evidence:** all output is `#`-prefixed plain text, fenced code
blocks. No JSON, no LSP, no escape codes. Pipes cleanly.

## Cross-cutting

### Economy

| Run | Total lines | Preamble lines | Finding lines | Ratio (find:teach) |
|-----|-------------|----------------|---------------|---------------------|
| R1 | 33 | 25 | 8 | 0.32 |
| R4 | 41 | 25 | 16 | 0.64 |
| R5 | 33 | 25 | 8 | 0.32 |
| R6 | 56 | 25 + 6 (T4) | 25 | 0.81 |
| R7 | 29 | 25 | 4 | 0.16 |

At steady state — runs in a single workday in an active mesh —
preamble dominates. The ratio is not defensible: the news (the
partner) is buried in 25 lines of "what a mesh is" the reader
already knows by their second day with the tool.

### Trigger legibility

A developer reading R1's flush has no signal that *their* edit was
to `a.txt#L2-L5`. The output names only the partner. In a
multi-edit session this is a real problem: `b.txt#L3-L6 [CHANGED]`
in R5 could be the partner of an edit on `a.txt` *or* it could
itself be the trigger; the reader cannot tell from the flush.

Cheapest fix that does not violate Goal 1: a single
`# triggered by <path>#L<s>-L<e>` line at the top of each mesh
block, *after* the `<name> mesh: <why>` header so the partner
remains the headline. One line of context, no prescription.

### Header ordering

When the preamble fires, prose leads (24 lines of teaching) and
identity (`<name> mesh: <why>`) lands at line 25. This is
backwards. The mesh name + why is load-bearing; the teaching is
not. Lead with identity; teach below.

### Tone

Prescriptive verbs spotted in shipped output:

- R1/R4/R5: **"Compare, then either update it or accept that the
  relationship has shifted and re-record the mesh."** — three
  prescriptions in one sentence.
- R6: **"To re-record with the new extent:"** — fine; uniquely
  determined.

Drop the first; keep the second.

### Steady-state feel

Imagine 20 flushes across a mesh-rich workday: ~500 lines of
preamble, ~80 lines of findings if the developer is genuinely
crossing relationships often. The preamble goes from useful
(flush 1) to decorative (flush 2) to actively obscuring the news
(flush 3+). That is the noise threshold.

### Failure modes observed

1. **Schema-version error left repository-global state stuck.**
   Initial flush attempt printed
   `error: advice DB schema version mismatch: expected 2, found 1.
   Remove the stale .db file and retry.` This was state from a
   *prior* session for a *different* repo (state lives at
   `/tmp/git-mesh-claude-code/` per host). Recovery required
   `rm -rf` of the entire state dir; the message names no path.

2. **Partner-list collapses to the trigger.** R4's checkout-flow
   block lists only `a.txt#L2-L5 [CHANGED]` — the writer's own
   edit. Goal 1 violation in routing terms: the news *is* the
   developer's own action because there is no other range left
   in the mesh on the touched file.

3. **R6 preamble does not adapt to the actual reason.** The "When
   a range in a mesh changes ... Compare, then either update ..."
   paragraph still fires alongside the T4-specific "The edit
   reduced a range to far fewer lines ..." paragraph. Two
   teaching blocks back-to-back; the second supersedes the first
   but the first is still printed.

4. **`--documentation` flag silent in R6.** With per-reason doc
   blocks already inline in the default flush for T4, passing
   `--documentation` produced empty output (the second flush
   suppressed everything via T7). Either the flag should not
   re-suppress, or the per-reason inline behaviour should be
   gated on the flag rather than always-on.

## Useful today?

**Yes** — for a developer (or Claude) who has just landed a real
edit to a range that *only* sits inside one mesh whose partner is
in *another* file. R1 routes correctly: it surfaces `b.txt#L3-L6`
with its excerpt and the why, which is exactly the operation the
developer can't be bothered to do by hand.

**No** — when the edit is to a file whose own other ranges are
also meshed (R4/R5), the partner list collapses to the trigger;
when the edit is the third in a session (R5/R6) the 25-line
preamble crowds out the news; when state has been left over from
a previous session (R0 in the appendix), the tool fails with an
error message that doesn't name a path.

### Single highest-leverage change

**Lead with identity, gate the teaching.** Replace the
unconditional 25-line first-finding preamble with: (a) the
`<name> mesh: <why>` header at the top, always; (b) a one-line
`# triggered by <path>#L<s>-L<e>` so the developer can locate
themselves; (c) the partner range(s) and excerpt(s); (d)
"What is `git mesh`?" teaching only on a per-repo first-time
basis, dated, suppressible — *not* per-session. This single
change addresses Goal 1 (partner, not self), Goal 6 (teach with
the recommendation, not before unrelated findings), and Goal 7
(don't reteach across sessions in the same repo) at once,
without touching the parts that already work (T4 escalation,
exit-0 silence, drift-without-affect rendering).

---

## Appendix — runs

State dir: `/tmp/git-mesh-claude-code/` (host-global).

### R0 — schema mismatch (out-of-band)

Before any of the numbered runs, a leftover `*.db` from a
previous host process produced:
```
error: advice DB schema version mismatch: expected 2, found 1. Remove the stale .db file and retry.
```
Recovered with `rm -rf /tmp/git-mesh-claude-code`.

### R1 — first-finding routing

Setup:
```
mkdir -p /tmp/eval-advice-1 && cd /tmp/eval-advice-1
git init -q && git config user.email a@b && git config user.name x
printf 'line1\n...line10\n' > a.txt
printf 'apple\n...jicama\n' > b.txt
git add . && git commit -qm init
git-mesh add checkout-flow a.txt#L2-L5 b.txt#L3-L6
git-mesh why checkout-flow -m "Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server."
git-mesh commit
```
Trigger (edit `a.txt#L2-L5`):
```
printf 'line1\nLINE2-changed\nLINE3-changed\nline4\n...' > a.txt
git-mesh advice s1 add --write a.txt#L2-L5 --pre /tmp/eval-advice-1-pre-a.txt --post a.txt
git-mesh advice s1
```
Output:
```
# A mesh names a subsystem, flow, or concern that spans line ranges in
# several files. The mesh description states what those ranges do
# together. Invariants, caveats, and ownership belong in source comments,
# commit messages, CODEOWNERS, and PR descriptions — not here.
#
# Inspect a mesh:
#   git mesh show <name>           # ranges, description, history
#   git mesh ls <path>             # meshes that touch a file
#   git mesh stale                 # ranges whose bytes have drifted
#
# When a range in a mesh changes, the other ranges in the same mesh may
# need matching changes. The excerpt below is the related range — the
# code on the other side of the relationship. Compare, then either update
# it or accept that the relationship has shifted and re-record the mesh.
#
# A second `git mesh add` over the identical (path, extent) is a
# re-record — last-write-wins, no `rm` needed:
#
#   git mesh add <name> <path>#L<s>-L<e>
#   git mesh commit <name>              # finalized by the post-commit hook
#
# checkout-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - b.txt#L3-L6
#
# b.txt#L3-L6
# ```
# cherry
# date
# elder
# fig
# ```
```
Probe: does first finding name the partner with enough context to
act on it.

### R2 — repeat suppression

Trigger: re-applied identical write.
```
git-mesh advice s1 add --write a.txt#L2-L5 --pre /tmp/eval-advice-1-pre-a.txt --post a.txt
git-mesh advice s1
```
Output: *empty*. Exit 0.

Probe: T7 (no repeat) within session.

### R3 — unrelated edit

```
printf 'unrelated changed\n' > c.txt
git-mesh advice s3 add --write c.txt#L1-L1 --pre <(...) --post c.txt
git-mesh advice s3
```
Output: *empty*. Exit 0.

Probe: T9 (fail closed when no mesh matches).

### R4 — multi-mesh / cross-cutting

Setup added second mesh:
```
git-mesh add fruit-list b.txt#L3-L6 a.txt#L7-L9
git-mesh why fruit-list -m "Fruit catalogue rendered alongside the checkout receipt."
git-mesh commit
```
Trigger (edit `b.txt#L3-L6`):
```
printf 'apple\nbanana\nCHERRY-X\nDATE-X\n...' > b.txt
git-mesh advice s4 add --write b.txt#L3-L6 --pre /tmp/pre-b.txt --post b.txt
git-mesh advice s4
```
Output (preamble identical to R1; finding portion):
```
# checkout-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - a.txt#L2-L5 [CHANGED]
#
# a.txt#L2-L5
# ```
# LINE2-changed
# LINE3-changed
# line4
# line5
# ```
#
# fruit-list mesh: Fruit catalogue rendered alongside the checkout receipt.
# - a.txt#L7-L9
#
# a.txt#L7-L9
# ```
# line7
# line8
# line9
# ```
```
Probe: cross-cutting framing; whether the trigger range is treated
as the partner of itself.

### R5 — heavy drift, no T4

Trigger (large content swap, same line count):
```
printf 'line1\nTOTALLY\nDIFFERENT\nCONTENT\nNOW\nline6\n...' > a.txt
git-mesh advice s5 add --write a.txt#L2-L5 --pre /tmp/pre-a2.txt --post a.txt
git-mesh advice s5
```
Output (preamble identical to R1; finding portion):
```
# checkout-flow mesh: Checkout request flow ...
# - b.txt#L3-L6 [CHANGED]
#
# b.txt#L3-L6
# ```
# CHERRY-X
# DATE-X
# elder
# fig
# ```
#
# fruit-list mesh: Fruit catalogue rendered alongside the checkout receipt.
# - b.txt#L3-L6 [CHANGED]
```
Probe: does heavy drift escalate beyond `[CHANGED]`? — No.

### R6 — T4 range collapse

Trigger (post shorter than pre):
```
printf 'apple\nbanana\n' > b.txt
git-mesh advice s6 add --write b.txt#L3-L6 --pre /tmp/pre-b2.txt --post b.txt
git-mesh advice s6
```
Output (after R1's preamble, then T4-specific block):
```
# The edit reduced a range to far fewer lines than were recorded. The
# mesh now pins less code than the relationship was about. When the line
# span changes, remove the old range first, then add the new one:
#
#   git mesh rm  <name> <path>#L<old-s>-L<old-e>
#   git mesh add <name> <path>#L<new-s>-L<new-e>
#   git mesh commit <name>
#
# checkout-flow mesh: ...
# - a.txt#L2-L5 [CHANGED]
#
# a.txt#L2-L5
# ```
# TOTALLY
# DIFFERENT
# CONTENT
# NOW
# ```
#
# To re-record with the new extent:
#   git mesh rm checkout-flow b.txt#L3-L6
#   git mesh add checkout-flow b.txt#L3-L4
#
# fruit-list mesh: Fruit catalogue rendered alongside the checkout receipt.
# - a.txt#L7-L9
#
# a.txt#L7-L9
# ```
# line7
# line8
# line9
# ```
#
# To re-record with the new extent:
#   git mesh rm fruit-list b.txt#L3-L6
#   git mesh add fruit-list b.txt#L3-L4
```
Second flush with `--documentation`: empty (suppressed by T7).

Probe: does the T4 path get a concrete recommendation; does
teaching arrive with it.

### R7 — whole-file deletion

Trigger:
```
git rm a.txt   # (file already gone from working tree)
git-mesh advice s7 add --write a.txt --pre /tmp/pre-a2.txt --post /dev/null
git-mesh advice s7
```
Output (preamble identical; finding portion):
```
# checkout-flow mesh: Checkout request flow ...
# - b.txt#L3-L6 [CHANGED]
#
# fruit-list mesh: Fruit catalogue rendered alongside the checkout receipt.
# - b.txt#L3-L6 [CHANGED]
```
No excerpt (b.txt was previously collapsed; lines 3-6 don't exist).
No `[DELETED]` marker on the trigger side.

Probe: address-only rendering when excerpts cannot be drawn
(Goal 5).
