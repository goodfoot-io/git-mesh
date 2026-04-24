#!/usr/bin/env bash
# PostToolUse hook: after an Edit/Write, inject the same related mesh context
# shape as UserPromptSubmit for the file that was edited.

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

repo_root="$(git rev-parse --show-toplevel)"
rel="${file#"$repo_root"/}"

mapfile -t meshes < <(meshes_for_path "$rel")
[[ "${#meshes[@]}" -eq 0 ]] && exit 0

lines=""
for m in "${meshes[@]}"; do
  [[ -z "$m" ]] && continue
  why="$(git mesh why "$m" 2>/dev/null || true)"
  summary="$(render_mesh_summary "$m")"
  stale="$(render_stale "$m")"

  [[ -n "$lines" ]] && lines+=$'\n'
  lines+="$m mesh: $why"$'\n'
  if [[ -n "$summary" ]]; then
    while IFS= read -r range_line; do
      [[ -z "$range_line" ]] && continue
      range="${range_line##* }"
      status="$(
        awk -v range="$range" '$NF == range && $1 != "FRESH" { print $1; exit }' <<<"$stale"
      )"
      if [[ -n "$status" ]]; then
        lines+="- $range [$status]"$'\n'
      else
        lines+="- $range"$'\n'
      fi
    done <<<"$summary"
  fi
done

[[ -z "$lines" ]] && exit 0

emit_additional_context "PostToolUse" "$lines"

# Example stdin:
# {"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/repo/src/api.ts"}}
#
# Example additionalContext:
# api-contract mesh: API charge contract is covered by its test.
# - src/api.ts#L1-L3 [CHANGED]
# - tests/api.test.ts#L1-L5
#
# request-schema mesh: Charge request schema is shared by client and server.
# - src/api.ts#L1-L3 [CHANGED]
# - server/routes.ts#L8-L21
