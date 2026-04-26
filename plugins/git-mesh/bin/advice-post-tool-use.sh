#!/usr/bin/env bash
# PostToolUse: resolve the repo from the tool's actual target — file
# path for Read/Edit/MultiEdit/Write/NotebookEdit, parsed `cd` and
# `git -C` targets for Bash — rather than from Claude's tracked cwd.
# This keeps advice anchored to the repo Claude actually touched even
# when cwd and the operative repo diverge (e.g. cwd=/workspace,
# `git -C /foo …`). Then render advice in each unique repo and emit one
# combined hook output.

set -uo pipefail
. "$(dirname "$0")/advice-common.sh"

read_hook_input

sid="$(hook_field '.session_id')"
[ -n "$sid" ] || exit 0
cwd="$(hook_field '.cwd')"
[ -n "$cwd" ] || cwd="$PWD"
tool="$(hook_field '.tool_name')"

# Collect candidate directories the tool may have operated in.
collect_dirs() {
  case "$tool" in
    Read|Edit|MultiEdit|Write)
      local fp; fp="$(hook_field '.tool_input.file_path')"
      [ -n "$fp" ] || return 0
      printf '%s\n' "$(dirname "$(abspath_against "$cwd" "$fp")")"
      ;;
    NotebookEdit)
      local fp; fp="$(hook_field '.tool_input.notebook_path')"
      [ -n "$fp" ] || return 0
      printf '%s\n' "$(dirname "$(abspath_against "$cwd" "$fp")")"
      ;;
    Bash)
      local cmd; cmd="$(hook_field '.tool_input.command')"
      bash_candidate_dirs "$cwd" "$cmd"
      ;;
    mcp__*)
      # MCP tools don't expose a uniform target; fall back to cwd.
      printf '%s\n' "$cwd"
      ;;
    *)
      return 0
      ;;
  esac
}

mapfile -t cands < <(collect_dirs)
[ "${#cands[@]}" -gt 0 ] || exit 0

# Resolve each candidate to a unique repo toplevel.
declare -A seen=()
roots=()
for d in "${cands[@]}"; do
  root="$(resolve_repo_root "$d")"
  [ -n "$root" ] || continue
  if [ -z "${seen[$root]:-}" ]; then
    seen[$root]=1
    roots+=("$root")
  fi
done
[ "${#roots[@]}" -gt 0 ] || exit 0

# For Read, record the read in the file's repo using a worktree-relative
# path so `git mesh advice <id> read` accepts it.
if [ "$tool" = "Read" ]; then
  fp_raw="$(hook_field '.tool_input.file_path')"
  if [ -n "$fp_raw" ]; then
    fp="$(abspath_against "$cwd" "$fp_raw")"
    file_root="$(resolve_repo_root "$(dirname "$fp")")"
    if [ -n "$file_root" ] && [ -e "$fp" ]; then
      offset="$(hook_field '.tool_input.offset')"
      limit="$(hook_field '.tool_input.limit')"
      rel="${fp#"$file_root"/}"
      spec="$rel"
      if [ -n "$offset" ] && [ -n "$limit" ]; then
        end=$((offset + limit - 1))
        spec="$rel#L${offset}-L${end}"
      fi
      (cd "$file_root" && git mesh advice "$sid" read "$spec" >/dev/null 2>&1) || true
    fi
  fi
fi

# Render advice for each unique repo, concatenate, emit one JSON.
combined=""
for root in "${roots[@]}"; do
  out="$(render_advice_in "$root" "$sid")"
  [ -n "$out" ] || continue
  if [ -n "$combined" ]; then
    combined="${combined}"$'\n\n'"$out"
  else
    combined="$out"
  fi
done

emit_advice_text PostToolUse "$combined"
exit 0
