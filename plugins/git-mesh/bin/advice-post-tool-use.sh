#!/usr/bin/env bash
# PostToolUse: for Read, record the read range; for edit/write tools the
# workspace tree itself changed and the next render's incr_delta picks
# it up. Either way, render advice and surface anything new.

set -euo pipefail
. "$(dirname "$0")/advice-common.sh"

read_hook_input
in_git_repo || exit 0

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
tool="$(hook_field '.tool_name')"

case "$tool" in
  Read)
    path="$(hook_field '.tool_input.file_path')"
    if [ -n "$path" ] && [ -e "$path" ]; then
      offset="$(hook_field '.tool_input.offset')"
      limit="$(hook_field '.tool_input.limit')"
      spec="$path"
      if [ -n "$offset" ] && [ -n "$limit" ]; then
        end=$((offset + limit - 1))
        spec="$path#L${offset}-L${end}"
      fi
      git mesh advice "$sid" read "$spec" >/dev/null 2>&1 || true
    fi
    ;;
  Edit|MultiEdit|Write|NotebookEdit|Bash)
    : # nothing to record; the worktree diff carries the change
    ;;
  mcp__*)
    : # MCP tools (e.g. VS Code execute_command) may mutate the worktree;
      # the next render's incr_delta picks up anything that did
    ;;
  *)
    exit 0
    ;;
esac

emit_advice PostToolUse "$sid"
exit 0
