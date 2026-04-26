#!/usr/bin/env bash
# Stop: final flush so anything crossed by the last assistant turn that
# no PostToolUse caught (e.g. staging changes from a Bash git command)
# still surfaces. Informational only — never blocks turn-end.

set -euo pipefail
. "$(dirname "$0")/advice-common.sh"

read_hook_input
in_git_repo || exit 0

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0

stop_reason="$(hook_field '.stop_reason')"
case "$stop_reason" in
  max_tokens|stop_sequence) exit 0 ;;
esac

emit_advice Stop "$sid"
exit 0
