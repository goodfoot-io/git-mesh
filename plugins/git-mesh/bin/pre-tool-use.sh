#!/usr/bin/env bash
# PreToolUse hook: when Claude is about to Edit/Write a file, inject the
# partner ranges + why for every mesh that touches the file.
#
# Advisory only — never blocks. Scopes to Edit/Write/NotebookEdit tools.

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

# Normalize to repo-relative path; git mesh ls accepts either.
repo_root="$(git rev-parse --show-toplevel)"
rel="${file#"$repo_root"/}"

mapfile -t meshes < <(meshes_for_path "$rel")
[[ "${#meshes[@]}" -eq 0 ]] && exit 0

{
  echo "git-mesh: $rel is covered by ${#meshes[@]} mesh(es). Partner ranges must stay in sync:"
  for m in "${meshes[@]}"; do
    echo ""
    echo "• $m"
    render_mesh_summary "$m" | sed 's/^/    /'
  done
  echo ""
  echo "Review partner ranges before editing. Run 'git mesh stale <name>' after to check drift."
} | {
  body="$(cat)"
  emit_additional_context "PreToolUse" "$body"
}
