#!/usr/bin/env bash
# Stop hook: flush any remaining mesh advice for the session.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

output="$(git mesh advice "$session_id" 2>/dev/null || true)"
[[ -n "$output" ]] && emit_stop_context "$output"
exit 0

# Example stdin:
# {"session_id":"abc123","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"Done."}
#
# Example output (systemMessage + additionalContext):
# # billing/checkout-request-flow mesh: Checkout request flow ...
# # - api/charge.ts#L30-L76 [CHANGED]
# # - web/checkout.tsx#L88-L120
