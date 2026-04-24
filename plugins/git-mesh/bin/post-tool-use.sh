#!/usr/bin/env bash
# PostToolUse hook: after an Edit/Write, report mesh drift for the file.
# If partner ranges are now out of sync, surface them so Claude opens them
# before claiming the task done.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=_lib.sh
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

output=""
for m in "${meshes[@]}"; do
  stale="$(render_stale "$m")"
  [[ -z "$stale" ]] && continue
  # Only surface meshes that report actual drift (CHANGED/MOVED/ORPHANED/...).
  if grep -Eq '^(CHANGED|MOVED|ORPHANED|MERGE_CONFLICT|SUBMODULE|CONTENT_UNAVAILABLE)' <<<"$stale"; then
    output+="• $m"$'\n'
    output+="$(sed 's/^/    /' <<<"$stale")"$'\n\n'
    partners="$(render_mesh_summary "$m")"
    [[ -n "$partners" ]] && output+="    partners: $partners"$'\n\n'
  fi
done

[[ -z "$output" ]] && exit 0

body="git-mesh drift after editing $rel — partner ranges may need review:"$'\n\n'"$output"
emit_additional_context "PostToolUse" "$body"
