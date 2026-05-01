#!/usr/bin/env bash
# PostToolUse: for `Read`, record an anchor read; for everything else,
# `flush <tool_use_id>` against the snapshot captured at PreToolUse and
# emit any newly-touched mesh advice as additionalContext for the next turn.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"
trap 'rm -f -- "${_ADVICE_DEBUG_FILE:-}" 2>/dev/null' EXIT

read_hook_input

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
tool="$(hook_field '.tool_name')"
tuid="$(hook_field '.tool_use_id')"

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

    if [ -n "$tuid" ]; then
      text="$(run_advice_verb "$file_root" "$sid" read "$anchor" "$tuid")"
    else
      text="$(run_advice_verb "$file_root" "$sid" read "$anchor")"
    fi
    emit_advice_text PostToolUse "$text"
    ;;

  Edit|MultiEdit)
    fp_raw="$(hook_field '.tool_input.file_path')"
    [ -n "$fp_raw" ] || exit 0
    [ -n "$tuid" ] || exit 0
    fp="$(abspath_against "$cwd" "$fp_raw")"
    root="$(resolve_repo_root "$(dirname "$fp")")"
    [ -n "$root" ] || exit 0
    rel="${fp#"$root"/}"

    text=""
    patch_json="$(printf '%s' "$HOOK_INPUT" | jq -c '.tool_response.structuredPatch // empty')"
    if [ -z "$patch_json" ] || [ "$patch_json" = "[]" ]; then
      text="$(run_advice_verb "$root" "$sid" touch "$tuid" "$rel" modified)"
    else
      whole_file=0
      while IFS= read -r hunk; do
        new_start="$(printf '%s' "$hunk" | jq -r '.newStart // empty')"
        new_lines="$(printf '%s' "$hunk" | jq -r '.newLines // empty')"
        [ -n "$new_start" ] && [ -n "$new_lines" ] || continue
        if [ "$new_lines" -eq 0 ]; then
          whole_file=1
          break
        fi
        end_line=$(( new_start + new_lines - 1 ))
        anchor="${rel}#L${new_start}-L${end_line}"
        hunk_text="$(run_advice_verb "$root" "$sid" touch "$tuid" "$anchor" modified)"
        text="${text}${hunk_text}"
      done < <(printf '%s' "$patch_json" | jq -c '.[]')
      if [ "$whole_file" -eq 1 ]; then
        text="$(run_advice_verb "$root" "$sid" touch "$tuid" "$rel" modified)"
      fi
    fi
    emit_advice_text PostToolUse "$text"
    ;;

  Write)
    fp_raw="$(hook_field '.tool_input.file_path')"
    [ -n "$fp_raw" ] || exit 0
    [ -n "$tuid" ] || exit 0
    fp="$(abspath_against "$cwd" "$fp_raw")"
    root="$(resolve_repo_root "$(dirname "$fp")")"
    [ -n "$root" ] || exit 0
    rel="${fp#"$root"/}"

    wtype="$(hook_field '.tool_response.type')"
    kind="modified"
    [ "$wtype" = "create" ] && kind="added"

    text="$(run_advice_verb "$root" "$sid" touch "$tuid" "$rel" "$kind")"
    emit_advice_text PostToolUse "$text"
    ;;

  *)
    [ -n "$tuid" ] || exit 0
    root="$(resolve_repo_root "$cwd")"
    [ -n "$root" ] || exit 0
    text="$(run_advice_verb "$root" "$sid" flush "$tuid")"
    emit_advice_text PostToolUse "$text"
    ;;
esac

exit 0
