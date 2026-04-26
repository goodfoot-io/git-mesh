#!/usr/bin/env bash
# Shared helpers for git-mesh advice hooks.
# Sourced by the per-event scripts under ${CLAUDE_PLUGIN_ROOT}/bin.

set -euo pipefail

# Read the hook payload once into $HOOK_INPUT.
read_hook_input() {
  HOOK_INPUT="$(cat)"
  export HOOK_INPUT
}

hook_field() {
  printf '%s' "$HOOK_INPUT" | jq -r "$1 // empty"
}

# Locate the repo for this hook invocation. Hooks fire from cwd; if cwd
# isn't in a git repo, exit silently — git mesh advice has nothing to do.
in_git_repo() {
  local cwd
  cwd="$(hook_field '.cwd')"
  [ -n "$cwd" ] || cwd="$PWD"
  cd "$cwd" 2>/dev/null || return 1
  git rev-parse --git-dir >/dev/null 2>&1
}

# Run `git mesh advice` and emit its stdout as additionalContext on the
# matching event. Silent when there is nothing to say. Failures of the
# binary (e.g. no baseline yet) are swallowed so a missing snapshot never
# blocks Claude.
emit_advice() {
  local event="$1" sid="$2" out
  out="$(git mesh advice "$sid" 2>/dev/null || true)"
  [ -n "$out" ] || return 0
  jq -nc --arg e "$event" --arg c "$out" \
    '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
}
