#!/usr/bin/env bash
# SessionStart: capture the workspace baseline that every later render
# diffs against. Runs on every source — including `compact`, which
# starts a fresh session id with no prior baseline of its own.

set -euo pipefail
. "$(dirname "$0")/advice-common.sh"

read_hook_input
in_git_repo || exit 0

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0

git mesh advice "$sid" snapshot >/dev/null 2>&1 || true
exit 0
