#!/usr/bin/env bash
# Stop hook: before Claude finishes its turn, check mesh drift across files
# modified in the worktree. If any mesh reports unacknowledged drift, inject
# a systemMessage summarizing it so the final response can't silently leave
# a contract partner out of sync.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
source "$HERE/_lib.sh"
require_env

# Files changed in the worktree relative to HEAD (staged or unstaged).
mapfile -t changed < <(git diff --name-only HEAD 2>/dev/null; git ls-files --others --exclude-standard 2>/dev/null)
[[ "${#changed[@]}" -eq 0 ]] && exit 0

declare -A seen=()
mesh_list=()
for f in "${changed[@]}"; do
  [[ -z "$f" ]] && continue
  while read -r m; do
    [[ -z "$m" ]] && continue
    [[ -n "${seen[$m]:-}" ]] && continue
    seen[$m]=1
    mesh_list+=("$m")
  done < <(meshes_for_path "$f")
done
[[ "${#mesh_list[@]}" -eq 0 ]] && exit 0

output=""
for m in "${mesh_list[@]}"; do
  stale="$(render_stale "$m")"
  [[ -z "$stale" ]] && continue
  if grep -Eq '^(CHANGED|MOVED|ORPHANED|MERGE_CONFLICT|SUBMODULE|CONTENT_UNAVAILABLE)' <<<"$stale" \
     && ! grep -q '(ack)' <<<"$stale"; then
    output+="• $m"$'\n'
    output+="$(sed 's/^/    /' <<<"$stale")"$'\n'
  fi
done

[[ -z "$output" ]] && exit 0

msg="git-mesh: unacknowledged drift in meshes touched this turn. Review partner ranges or re-anchor before declaring the task done:"$'\n\n'"$output"
jq -cn --arg m "$msg" '{systemMessage: $m}'
