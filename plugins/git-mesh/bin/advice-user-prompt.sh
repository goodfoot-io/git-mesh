#!/usr/bin/env bash
# UserPromptSubmit: scan the prompt for path-shaped tokens that resolve
# in the current worktree, record each as a `read` event, then render
# the resulting advice as additionalContext. The advice engine's
# fingerprint cache (advice-seen.jsonl) suppresses repeats across
# renders, so we don't need to dedup here.

set -euo pipefail
. "$(dirname "$0")/advice-common.sh"

read_hook_input
in_git_repo || exit 0

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
prompt="$(hook_field '.prompt')"
[ -n "$prompt" ] || { emit_advice UserPromptSubmit "$sid"; exit 0; }

# Extract whitespace-delimited tokens that look like paths (contain `/`
# or a `.ext` suffix), strip surrounding punctuation, drop absolute /
# URL / flag tokens, and keep ones that resolve in the worktree.
candidate_paths() {
  printf '%s\n' "$prompt" \
    | tr -s '[:space:]' '\n' \
    | sed -E 's/^[`"'"'"'(\[<]+//; s/[`"'"'"'),.;:>\]]+$//' \
    | awk '/\// || /\.[A-Za-z0-9]+$/' \
    | while IFS= read -r tok; do
        [ -n "$tok" ] || continue
        case "$tok" in
          /*|http*|-*) continue ;;
        esac
        [ -e "$tok" ] && printf '%s\n' "$tok"
      done \
    | sort -u
}

paths="$(candidate_paths)"
if [ -n "$paths" ]; then
  printf '%s\n' "$paths" \
    | xargs -d '\n' -r git mesh advice "$sid" read >/dev/null 2>&1 || true
fi

emit_advice UserPromptSubmit "$sid"
exit 0
