#!/usr/bin/env bash
# PostToolUse hook: after an Edit/Write, record the write event and flush
# mesh advice for the edited file.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
tool="$(jq -r '.tool_name // empty' <<<"$payload")"

case "$tool" in
  Edit|Write|NotebookEdit) ;;
  *) exit 0 ;;
esac

file="$(jq -r '.tool_input.file_path // empty' <<<"$payload")"
[[ -z "$file" ]] && exit 0

session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

repo_root="$(git rev-parse --show-toplevel)"
rel="${file#"$repo_root"/}"

git mesh advice "$session_id" add --write "$rel" 2>/dev/null || true
output="$(git mesh advice "$session_id" 2>/dev/null || true)"
[[ -n "$output" ]] && emit_additional_context "PostToolUse" "$output"
exit 0

# Example stdin:
# {"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/repo/api/charge.ts"}}
#
# Example additionalContext:
# # billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# # - api/charge.ts#L30-L76 [CHANGED]
# # - web/checkout.tsx#L88-L120
