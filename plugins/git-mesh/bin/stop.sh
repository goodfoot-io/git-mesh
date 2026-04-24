#!/usr/bin/env bash
# Stop hook: compare current git-mesh drift against the SessionStart baseline
# and inject only mesh ranges that became non-FRESH during this Claude session.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

baseline_path="$(session_cache_path "$session_id")"
current="$(git mesh stale --format=porcelain --no-exit-code 2>/dev/null || true)"
[[ -z "$current" ]] && exit 0

baseline=""
[[ -f "$baseline_path" ]] && baseline="$(cat "$baseline_path")"

new_findings="$(
  awk '
    BEGIN { FS = "\t" }
    FNR == NR {
      if ($0 !~ /^#/ && NF >= 6) {
        baseline[$3 "\t" $4 "\t" $5 "\t" $6] = 1
      }
      next
    }
    $0 !~ /^#/ && NF >= 6 {
      key = $3 "\t" $4 "\t" $5 "\t" $6
      if (!(key in baseline)) {
        print $1 "\t" key
      }
    }
  ' <(printf '%s\n' "$baseline") <(printf '%s\n' "$current")
)"
[[ -z "$new_findings" ]] && exit 0

declare -A stale_ranges_by_mesh=()
declare -A seen_mesh=()
mesh_list=()
while IFS=$'\t' read -r status mesh path start end; do
  [[ -z "$mesh" || -z "$path" || -z "$start" || -z "$end" ]] && continue
  range="$path#L$start-L$end"
  stale_ranges_by_mesh["$mesh"$'\t'"$range"]="$status"
  if [[ -z "${seen_mesh[$mesh]:-}" ]]; then
    seen_mesh[$mesh]=1
    mesh_list+=("$mesh")
  fi
done <<<"$new_findings"
[[ "${#mesh_list[@]}" -eq 0 ]] && exit 0

lines=""
for mesh in "${mesh_list[@]}"; do
  why="$(git mesh why "$mesh" 2>/dev/null || true)"
  summary="$(render_mesh_summary "$mesh")"
  [[ -z "$summary" ]] && continue

  [[ -n "$lines" ]] && lines+=$'\n'
  lines+="$mesh mesh: $why"$'\n'
  while IFS= read -r range_line; do
    [[ -z "$range_line" ]] && continue
    range="${range_line##* }"
    status="${stale_ranges_by_mesh["$mesh"$'\t'"$range"]:-}"
    if [[ -n "$status" ]]; then
      lines+="- $range [$status]"$'\n'
    else
      lines+="- $range"$'\n'
    fi
  done <<<"$summary"
done

[[ -z "$lines" ]] && exit 0

emit_stop_context "$lines"

# Example stdin:
# {"session_id":"abc123","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"Done."}
#
# Example additionalContext:
# billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - web/checkout.tsx#L88-L120
# - api/charge.ts#L30-L76 [CHANGED]
