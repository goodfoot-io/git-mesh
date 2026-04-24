#!/usr/bin/env bash
# UserPromptSubmit hook: scan the prompt for repo-relative file paths and
# inject a compact list of meshes touching them, so Claude has the
# relationship context before its first tool call.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
prompt="$(jq -r '.prompt // empty' <<<"$payload")"
[[ -z "$prompt" ]] && exit 0

repo_root="$(git rev-parse --show-toplevel)"

# Extract plausible path tokens: contain a slash or a dotted extension,
# no spaces, reasonable length. Dedupe.
mapfile -t candidates < <(
  grep -oE '[A-Za-z0-9_./-]+\.[A-Za-z0-9]+(#L[0-9]+-L[0-9]+)?|[A-Za-z0-9_./-]+/[A-Za-z0-9_./-]+' <<<"$prompt" \
    | awk '!seen[$0]++'
)
[[ "${#candidates[@]}" -eq 0 ]] && exit 0

declare -A seen_mesh=()
lines=""
for token in "${candidates[@]}"; do
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
  while read -r m; do
    [[ -z "$m" ]] && continue
    [[ -n "${seen_mesh[$m]:-}" ]] && continue
    seen_mesh[$m]=1
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
  done < <(meshes_for_path "$token")
done

[[ -z "$lines" ]] && exit 0

emit_additional_context "UserPromptSubmit" "$lines"

# Example stdin:
# {"hook_event_name":"UserPromptSubmit","prompt":"Please edit api/charge.ts."}
#
# Example additionalContext:
# billing/checkout-request-flow mesh: Checkout request flow that carries a charge attempt from the browser to the Stripe-backed server.
# - web/checkout.tsx#L88-L120
# - api/charge.ts#L30-L76 [CHANGED]
#
# billing/charge-amount-contract mesh: Charge amount contract shared between the browser payload and the server-side Stripe call.
# - api/charge.ts#L30-L76 [CHANGED]
# - server/stripe.ts#L12-L44
