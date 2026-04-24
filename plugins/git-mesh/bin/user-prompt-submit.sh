#!/usr/bin/env bash
# UserPromptSubmit hook: scan the prompt for repo-relative file paths and
# inject a compact list of meshes touching them, so Claude has the
# relationship context before its first tool call.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
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
    summary="$(render_mesh_summary "$m")"
    lines+="• $m — $summary"$'\n'
  done < <(meshes_for_path "$token")
done

[[ -z "$lines" ]] && exit 0

body="git-mesh: relationships covering files mentioned in this prompt:"$'\n\n'"$lines"
emit_additional_context "UserPromptSubmit" "$body"
