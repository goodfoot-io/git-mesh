#!/usr/bin/env bash
# PostToolUse hook: after an Edit/Write, run a bare advice render so
# Claude sees mesh advice for the workspace delta. Writes are no longer
# explicitly recorded — the file-backed pipeline derives them by diffing
# the workspace tree against `last-flush.objects`.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
tool="$(jq -r '.tool_name // empty' <<<"$payload")"

case "$tool" in
  Edit|MultiEdit|Write|NotebookEdit) ;;
  *) exit 0 ;;
esac

session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

# Force pending writes from the just-finished editor tool to land on
# disk before workspace_tree::capture reads them. Without this, the
# editor's buffered write can race the immediate render and the edit
# does not surface until a later render — advice attribution drifts.
# `sync` is POSIX and cheap (sub-second on any reasonable filesystem);
# absence is treated as a soft fall-back rather than a hard skip.
if command -v sync >/dev/null 2>&1; then
  sync
fi

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
