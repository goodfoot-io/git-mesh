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

session_cache_dir() {
  printf '%s\n' "/tmp/git-mesh-claude-code"
}

# Emit a PreToolUse/PostToolUse/UserPromptSubmit additionalContext payload.
emit_additional_context() {
  local event="$1" body="$2"
  [[ -z "$body" ]] && exit 0
  jq -cn --arg e "$event" --arg c "$body" \
    '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
}

# Emit Stop context in both places Claude Code understands for a passive
# warning: top-level systemMessage, plus hookSpecificOutput.additionalContext
# with the same body so the context injection mirrors the other hooks.
emit_stop_context() {
  local body="$1"
  [[ -z "$body" ]] && exit 0
  jq -cn --arg c "$body" \
    '{systemMessage: $c, hookSpecificOutput: {hookEventName: "Stop", additionalContext: $c}}'
}
