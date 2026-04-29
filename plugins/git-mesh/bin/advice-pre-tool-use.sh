#!/usr/bin/env bash
# PreToolUse: capture a per-tool_use_id snapshot pair so PostToolUse can
# attribute working-tree changes back to this exact tool call. No-ops for
# the read-only deny-list.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"
trap 'rm -f -- "${_ADVICE_DEBUG_FILE:-}" 2>/dev/null' EXIT

read_hook_input

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
tuid="$(hook_field '.tool_use_id')"
[ -n "$tuid" ] || exit 0
tool="$(hook_field '.tool_name')"

case "$tool" in
  Read|Grep|Glob|LS|WebFetch|WebSearch) exit 0 ;;
esac

cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
root="$(resolve_repo_root "$cwd")"
[ -n "$root" ] || exit 0

(cd "$root" && git mesh advice "$sid" mark "$tuid" >/dev/null 2>&1) || true
exit 0
