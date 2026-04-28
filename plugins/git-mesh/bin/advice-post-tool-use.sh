#!/usr/bin/env bash
# PostToolUse: dispatch Read → `read <anchor>`; everything else → `milestone`.
# No per-tool branching beyond Read vs everything-else; no path parsing for
# Bash/mcp or other tools. Fail-open — internal errors silently exit 0.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"
trap 'rm -f -- "${_ADVICE_DEBUG_FILE:-}" 2>/dev/null' EXIT

read_hook_input

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
tool="$(hook_field '.tool_name')"

case "$tool" in
  Read)
    fp_raw="$(hook_field '.tool_input.file_path')"
    [ -n "$fp_raw" ] || exit 0
    fp="$(abspath_against "$cwd" "$fp_raw")"
    file_root="$(resolve_repo_root "$(dirname "$fp")")"
    [ -n "$file_root" ] || exit 0

    offset="$(hook_field '.tool_input.offset')"
    limit="$(hook_field '.tool_input.limit')"
    rel="${fp#"$file_root"/}"
    anchor="$rel"
    if [ -n "$offset" ] && [ -n "$limit" ]; then
      end=$((offset + limit - 1))
      anchor="${rel}#L${offset}-L${end}"
    fi

    text="$(run_advice_verb "$file_root" "$sid" read "$anchor")"
    emit_advice_text PostToolUse "$text"
    ;;

  *)
    root="$(resolve_repo_root "$cwd")"
    [ -n "$root" ] || exit 0
    text="$(run_advice_verb "$root" "$sid" milestone)"
    emit_advice_text PostToolUse "$text"
    ;;
esac

exit 0
