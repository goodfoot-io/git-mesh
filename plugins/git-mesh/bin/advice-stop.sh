#!/usr/bin/env bash
# Stop: call `git mesh advice <sid> stop` so the CLI can emit any final
# mesh-tracking notice. Informational only — never blocks turn-end.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"
trap 'rm -f -- "${_ADVICE_DEBUG_FILE:-}" 2>/dev/null' EXIT

read_hook_input
in_git_repo || exit 0

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0

stop_reason="$(hook_field '.stop_reason')"
case "$stop_reason" in
  max_tokens|stop_sequence) exit 0 ;;
esac

cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
root="$(resolve_repo_root "$cwd")"
[ -n "$root" ] || exit 0

text="$(run_advice_verb "$root" "$sid" stop)"
emit_advice_text Stop "$text"
exit 0
