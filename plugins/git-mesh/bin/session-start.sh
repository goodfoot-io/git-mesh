#!/usr/bin/env bash
# SessionStart hook: record a snapshot event and flush any pre-existing mesh
# advice so Claude has context before its first tool call.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

git mesh advice "$session_id" snapshot 2>/dev/null || true
output="$(git mesh advice "$session_id" 2>/dev/null || true)"
[[ -n "$output" ]] && emit_additional_context "SessionStart" "$output"
exit 0

# Example stdin:
# {"session_id":"abc123","transcript_path":"/Users/.../.claude/projects/.../00893aaf-19fa-41d2-8238-13269b9b3ca0.jsonl","cwd":"/repo","hook_event_name":"SessionStart","source":"startup","model":"claude-sonnet-4-6"}
#
# Example additionalContext (only if pre-existing mesh findings):
# # billing/checkout mesh: Checkout request flow ...
# # - api.ts#L1-L10
# # - web/checkout.tsx#L5-L20
