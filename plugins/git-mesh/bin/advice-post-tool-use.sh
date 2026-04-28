#!/usr/bin/env bash
# PostToolUse: dispatch Read → `read <anchor>` and Bash/mcp__* → `milestone`.
# Read resolves a single anchor from file_path + offset/limit; Bash/mcp run
# milestone once per unique repo root found via bash_candidate_dirs (or cwd
# for mcp__*). Fail-open — internal errors silently exit 0.

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

  Bash)
    cmd="$(hook_field '.tool_input.command')"
    declare -A seen=()
    roots=()
    while IFS= read -r d; do
      root="$(resolve_repo_root "$d")"
      [ -n "$root" ] || continue
      if [ -z "${seen[$root]:-}" ]; then
        seen[$root]=1
        roots+=("$root")
      fi
    done < <(bash_candidate_dirs "$cwd" "$cmd")
    [ "${#roots[@]}" -gt 0 ] || exit 0

    combined=""
    for root in "${roots[@]}"; do
      out="$(run_advice_verb "$root" "$sid" milestone)"
      [ -n "$out" ] || continue
      if [ -n "$combined" ]; then
        combined="${combined}"$'\n\n'"$out"
      else
        combined="$out"
      fi
    done
    emit_advice_text PostToolUse "$combined"
    ;;

  mcp__*)
    root="$(resolve_repo_root "$cwd")"
    [ -n "$root" ] || exit 0
    text="$(run_advice_verb "$root" "$sid" milestone)"
    emit_advice_text PostToolUse "$text"
    ;;

  *)
    exit 0
    ;;
esac

exit 0
