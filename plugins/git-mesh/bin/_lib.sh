#!/usr/bin/env bash
# Shared helpers for git-mesh Claude Code hooks.
# Sourced by sibling scripts; not invoked directly.

set -euo pipefail

have() { command -v "$1" >/dev/null 2>&1; }

# Exit 0 silently if the environment can't support the hook.
require_env() {
  have git      || exit 0
  have git-mesh || have_mesh_subcommand || exit 0
  have jq       || exit 0
  git rev-parse --git-dir >/dev/null 2>&1 || exit 0
}

have_mesh_subcommand() {
  git mesh --help >/dev/null 2>&1
}

# Emit a PreToolUse/PostToolUse/UserPromptSubmit additionalContext payload.
emit_additional_context() {
  local event="$1" body="$2"
  [[ -z "$body" ]] && exit 0
  jq -cn --arg e "$event" --arg c "$body" \
    '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
}

# List mesh names whose ranges touch the given path (or path#Lstart-Lend).
# Falls back to parsing human output when --format=json is unavailable.
meshes_for_path() {
  local path="$1"
  [[ -z "$path" ]] && return 0
  git mesh ls "$path" --format=json 2>/dev/null \
    | jq -r '.[]?.name // empty' 2>/dev/null \
    || git mesh ls "$path" 2>/dev/null \
       | awk 'NF && $1 !~ /^#/ {print $1}'
}

# Render a compact summary for one mesh: "name: why -> partner ranges".
render_mesh_summary() {
  local name="$1"
  git mesh "$name" --oneline 2>/dev/null || true
}

# Render drift findings for a mesh, HEAD+Index+Worktree layers, no exit code.
render_stale() {
  local name="$1"
  git mesh stale "$name" --no-exit-code --oneline 2>/dev/null || true
}
