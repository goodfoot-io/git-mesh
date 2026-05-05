# Using `git mesh advice`

Advice is a session-scoped stream that surfaces the implicit semantic dependencies a developer crosses while working. Each render emits one candidate per coupling crossed since the last flush — a mesh anchor read, a related anchor that drifted under an edit, a rename that broke an anchored path, sibling anchors co-touched in the session, staging that cuts across the mesh — and carries the mesh's why so the developer reads which subsystem the anchors form at the moment they're stepping on it. The related anchor the candidate routes to may be code or prose: an ADR section, a contract clause, a runbook step, an API doc are normal advice targets.

Advice is observation, not enforcement. It doesn't gate commits and doesn't run in CI. Drift gating belongs to `git mesh stale` and the `pre-commit` subcommand; advice is the *during-work* surface that shows the developer which dependencies they've touched.

## Session lifecycle

A session is identified by a `<sessionId>` chosen by the caller (an editor, an agent harness, a shell wrapper). Allowed characters: ASCII letters, digits, `-`, `_`, `.`. Anything else (path separators, whitespace, NUL, control chars) is rejected — the id maps directly onto a per-session directory and silent rewrites would collide distinct ids.

```bash
# 1. Set a baseline. Captures the current workspace tree as the diff origin.
git mesh advice my-session snapshot

# 2. (Optional) Record reads as they happen. A read crosses a dependency
#    even when no edit is made, so editors and agents should log opens.
git mesh advice my-session read 'web/checkout.tsx#L88-L120' api/charge.ts

# 3. Bare render. Diffs the current tree against the baseline and the
#    last flush, walks staging and recorded reads, and emits candidates.
git mesh advice my-session
```

Each bare render advances the last-flush state, so the next render only surfaces couplings crossed *since* this one. Candidates already shown in the session are suppressed by their fingerprint; whole-file pins reflect blob ids so successive edits to the same meshed file resurface routing to the other anchors in the mesh instead of being suppressed by content-blind fingerprints.

## Required baseline

A bare render or a `read` against a session with no `baseline.state` fails closed:

```
no baseline for session `my-session`; run snapshot first (`git mesh advice my-session snapshot`)
```

Run `snapshot` once at session start. Re-snapshot when the session's notion of "starting point" should reset (new branch, fresh agent turn, after a long idle).

## Documentation mode

`--documentation` appends per-reason explanation blocks the first time a reason kind is rendered for a session, then suppresses them on later flushes via `docs-seen.jsonl`. Bare renders without the flag never record topic keys, so flipping `--documentation` on later still shows the docs once.

```bash
git mesh advice my-session --documentation
```

Use this when a developer or agent is new to advice output and needs the per-reason explanations; omit it for steady-state work.

## Reading candidates

Candidates surface five kinds of crossing — read-intersects-mesh, delta-intersects-mesh, related-anchor drift, rename consequence, anchor shrink, session co-touch, and staging cross-cut. Markers and routing are documented in `./reading-stale-output.md`; the dependency-level question advice answers is "which coupling did I just touch?" — and the answer is the mesh's why, rendered alongside the affected anchors.

When a candidate fires:

- **Read or open the related anchor** the candidate routes to. The anchor address plus the subsystem definition is usually enough to orient you; the related file (code or prose) shows the mechanism.
- **If the subsystem itself has changed** (the anchors no longer form what the why says they do), update the why (`git mesh why <name> -m …`) as part of the same change.
- **If the subsystem is the same but the anchors drifted**, see `./responding-to-drift.md`.

## Common quirks

- **`last-flush` inconsistent with objects**: a crash between the rename and the state write can leave a stale state pointing at a tree that's no longer in objects. The next render falls back to baseline diff and prints a one-line note on stderr; no action needed.
- **Stdout broken pipe**: cache-correctness invariants (the last-flush rename and state write) run before stdout, so an EPIPE doesn't corrupt the cache. Seen-set advances happen only on stdout success or EPIPE; any other write failure leaves candidates to resurface next render.
- **Internal advice paths**: paths under the per-session store directory are filtered out of touch intervals automatically — the session's own writes don't trigger advice on themselves.

## When advice belongs vs when it doesn't

Advice is the right surface when the question is "which dependencies have I touched in this session, and what do they form?" — i.e. observational, per-developer, scoped to a unit of work.

Advice is the wrong surface for:
- **Drift gating.** Use `git mesh stale` and `git mesh pre-commit`.
- **Authoring decisions** (re-anchor, fix the related anchor, update why). Advice points at the coupling; `./responding-to-drift.md` covers what to do.
- **Cross-developer signal**. Sessions are local; advice does not aggregate across machines.
