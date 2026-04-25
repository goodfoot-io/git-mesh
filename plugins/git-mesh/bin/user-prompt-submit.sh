#!/usr/bin/env bash
# UserPromptSubmit hook: scan the prompt for repo-relative file paths,
# record read events, and flush mesh advice so Claude has context first.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
prompt="$(jq -r '.prompt // empty' <<<"$payload")"
[[ -z "$prompt" ]] && exit 0

session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

repo_root="$(git rev-parse --show-toplevel)"

# Extract plausible path tokens: contain a slash or a dotted extension,
# no spaces, reasonable length. Dedupe.
mapfile -t candidates < <(
  grep -oE '[A-Za-z0-9_./-]+\.[A-Za-z0-9]+(#L[0-9]+-L[0-9]+)?|[A-Za-z0-9_./-]+/[A-Za-z0-9_./-]+' <<<"$prompt" \
    | awk '!seen[$0]++'
)
[[ "${#candidates[@]}" -eq 0 ]] && exit 0

added=0
for token in "${candidates[@]}"; do
  # Strip trailing punctuation
  while :; do
    case "$token" in
      *.|*,|*\;|*:|*\)|*\]|*\}) token="${token%?}" ;;
      *) break ;;
    esac
  done
  # Strip any #L... for existence check
  path="${token%%#*}"
  # Only accept paths that actually exist under the repo.
  if [[ ! -e "$repo_root/$path" && ! -e "$path" ]]; then
    continue
  fi
  git mesh advice "$session_id" add --read "$path" 2>/dev/null || true
  added=1
done

[[ "$added" -eq 0 ]] && exit 0

output="$(git mesh advice "$session_id" 2>/dev/null || true)"
[[ -n "$output" ]] && emit_additional_context "UserPromptSubmit" "$output"
exit 0

# Example stdin:
# {"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"Please edit api/charge.ts."}
#
# Example additionalContext:
# # billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# # - api/charge.ts#L30-L76
# # - web/checkout.tsx#L88-L120
