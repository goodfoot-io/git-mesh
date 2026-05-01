#!/usr/bin/env bash
# SessionEnd: remove the session advice directory and any leftover snapshot pairs.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"
trap 'rm -f -- "${_ADVICE_DEBUG_FILE:-}" 2>/dev/null' EXIT

read_hook_input

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
root="$(resolve_repo_root "$cwd")"
[ -n "$root" ] || exit 0

run_advice_verb "$root" "$sid" end >/dev/null 2>&1 || true
exit 0
