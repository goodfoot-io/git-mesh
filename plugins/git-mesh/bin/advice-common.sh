#!/usr/bin/env bash
# Shared helpers for git-mesh advice hooks.
# Sourced by the per-event scripts under ${CLAUDE_PLUGIN_ROOT}/bin.

set -uo pipefail

# When GIT_MESH_ADVICE_DEBUG=1, collect all CLI stderr into a temp file so it
# can be mirrored into systemMessage. The file is created here (at source time)
# so it outlives the $() subshells used in render_advice_in / emit_advice.
# The caller is responsible for removing the file; we do not set an EXIT trap
# here because subshells created by $() inherit traps and would delete the file
# before the parent process can read it.
if [ "${GIT_MESH_ADVICE_DEBUG:-0}" = "1" ]; then
  _ADVICE_DEBUG_FILE="$(mktemp)"
  export _ADVICE_DEBUG_FILE
else
  _ADVICE_DEBUG_FILE=""
  export _ADVICE_DEBUG_FILE
fi

# Hooks are informational; an internal failure must never block Claude's
# turn or surface as a non-blocking exit-code error in the transcript.
# Trap any uncaught error, write a breadcrumb to stderr, and exit 0.
_advice_hook_err() {
  local rc=$? line=$1
  printf 'git-mesh advice hook: error rc=%s at line %s in %s\n' \
    "$rc" "$line" "${BASH_SOURCE[1]:-?}" >&2
  exit 0
}
trap '_advice_hook_err $LINENO' ERR

# Read the hook payload once into $HOOK_INPUT.
read_hook_input() {
  HOOK_INPUT="$(cat)"
  export HOOK_INPUT
}

hook_field() {
  printf '%s' "$HOOK_INPUT" | jq -r "$1 // empty"
}

# Locate the repo for this hook invocation. Hooks fire from cwd; if cwd
# isn't in a git repo, exit silently — git mesh advice has nothing to do.
in_git_repo() {
  local cwd
  cwd="$(hook_field '.cwd')"
  [ -n "$cwd" ] || cwd="$PWD"
  cd "$cwd" 2>/dev/null || return 1
  git rev-parse --git-dir >/dev/null 2>&1
}

# Map a directory to its containing git repo toplevel, or empty if the
# directory isn't inside a working tree.
resolve_repo_root() {
  local dir="$1"
  [ -n "$dir" ] && [ -d "$dir" ] || return 0
  (cd "$dir" 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null) || true
}

# Resolve $2 against $1 if relative; pass through if absolute.
abspath_against() {
  case "$2" in
    /*) printf '%s\n' "$2" ;;
    *)  printf '%s\n' "$1/$2" ;;
  esac
}

# Print every directory a Bash command may operate in: the inherited cwd
# plus every `cd <dir>` and `git -C <dir>` target found in the command
# string. Heuristic — handles `cd X`, `cd X &&`, `cd X;`, `(cd X && …)`,
# and `git -C X <subcmd>`. Subshell-only `cd`s still surface, which is
# the safer side to err on (we'd rather render advice for a repo Claude
# briefly visited than miss a repo it actually mutated).
bash_candidate_dirs() {
  local cwd="$1" cmd="$2"
  printf '%s\n' "$cwd"
  # `cd <dir>` targets — `|| true` so an unmatched grep doesn't trip set -e.
  { printf '%s\n' "$cmd" \
      | grep -oE '(^|[[:space:];&|()])cd[[:space:]]+[^[:space:];&|()]+' \
      || true; } \
    | sed -E 's/^.*cd[[:space:]]+//' \
    | while IFS= read -r d; do
        [ -n "$d" ] && abspath_against "$cwd" "$d"
      done
  # `git -C <dir>` targets.
  { printf '%s\n' "$cmd" \
      | grep -oE '(^|[[:space:];&|()])git[[:space:]]+-C[[:space:]]+[^[:space:];&|()]+' \
      || true; } \
    | sed -E 's/^.*git[[:space:]]+-C[[:space:]]+//' \
    | while IFS= read -r d; do
        [ -n "$d" ] && abspath_against "$cwd" "$d"
      done
}

# Render advice for one repo and print the raw text (no JSON wrapper).
# Silent if there's nothing to say or no baseline yet.
# When GIT_MESH_ADVICE_DEBUG=1, appends stderr to _ADVICE_DEBUG_FILE if set.
render_advice_in() {
  local repo_root="$1" sid="$2"
  if [ "${GIT_MESH_ADVICE_DEBUG:-0}" = "1" ] && [ -n "${_ADVICE_DEBUG_FILE:-}" ]; then
    (cd "$repo_root" && git mesh advice "$sid" --snapshot-if-missing --documentation \
      2>>"$_ADVICE_DEBUG_FILE") || true
  else
    (cd "$repo_root" && git mesh advice "$sid" --snapshot-if-missing --documentation \
      2>/dev/null) || true
  fi
}

# Wrap rendered advice text in the hook output JSON, mirroring it into
# both additionalContext (for Claude's next turn) and systemMessage (for
# the transcript). Silent if the text is empty.
# When GIT_MESH_ADVICE_DEBUG=1 and _ADVICE_DEBUG_FILE is non-empty, appends
# the captured trace to systemMessage only.
emit_advice_text() {
  local event="$1" text="$2"
  [ -n "$text" ] || return 0
  local sys="$text"
  if [ "${GIT_MESH_ADVICE_DEBUG:-0}" = "1" ] && [ -n "${_ADVICE_DEBUG_FILE:-}" ] && [ -s "${_ADVICE_DEBUG_FILE}" ]; then
    local debug_content
    debug_content="$(cat "$_ADVICE_DEBUG_FILE")"
    sys="${text}"$'\n\n--- git-mesh-advice-debug ---\n'"${debug_content}"
  fi
  # Only PreToolUse, UserPromptSubmit, PostToolUse, and PostToolBatch
  # accept hookSpecificOutput.additionalContext. Stop (and any other
  # event) must use only top-level fields like systemMessage.
  case "$event" in
    PreToolUse|UserPromptSubmit|PostToolUse|PostToolBatch|SessionStart)
      jq -nc --arg e "$event" --arg c "$text" --arg s "$sys" \
        '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}, systemMessage: $s}'
      ;;
    *)
      jq -nc --arg s "$sys" '{systemMessage: $s}'
      ;;
  esac
}

# Convenience: render advice for a single repo (cwd) and emit JSON.
emit_advice() {
  local event="$1" sid="$2"
  if [ "${GIT_MESH_ADVICE_DEBUG:-0}" = "1" ] && [ -n "${_ADVICE_DEBUG_FILE:-}" ]; then
    local text
    text="$(git mesh advice "$sid" --documentation 2>>"$_ADVICE_DEBUG_FILE" || true)"
    emit_advice_text "$event" "$text"
  else
    emit_advice_text "$event" "$(git mesh advice "$sid" --documentation 2>/dev/null || true)"
  fi
}
