#!/usr/bin/env bash
# End-to-end tests for the git-mesh Claude Code hooks.
#
# Uses a real throwaway git repo and a real `git-mesh` binary on PATH. No
# mocks: every assertion is a real stdin/stdout/exit-code check against the
# hook scripts as Claude Code would invoke them.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
POST_TOOL="$HERE/post-tool-use.sh"
PROMPT="$HERE/user-prompt-submit.sh"
SESSION_START="$HERE/session-start.sh"
STOP="$HERE/stop.sh"

for tool in git git-mesh jq; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "missing required tool on PATH: $tool" >&2
    exit 2
  }
done

WORK="$(mktemp -d)"
CACHE_DIR="$(mktemp -d)"
# Redirect the hooks' on-disk cache into a sandbox so parallel runs don't
# collide with /tmp/git-mesh-claude-code.
export TMPDIR="$CACHE_DIR"
# _lib.sh hard-codes /tmp/git-mesh-claude-code; we keep sessions unique via
# random IDs so collisions with concurrent runs are effectively impossible.
HOOK_CACHE="/tmp/git-mesh-claude-code"

cleanup() { rm -rf "$WORK" "$CACHE_DIR"; }
trap cleanup EXIT

pass=0
fail=0

check() {
  local name="$1" condition="$2"
  if eval "$condition"; then
    printf 'ok   %s\n' "$name"
    pass=$((pass + 1))
  else
    printf 'FAIL %s\n  condition: %s\n' "$name" "$condition"
    fail=$((fail + 1))
  fi
}

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    printf 'ok   %s\n' "$name"
    pass=$((pass + 1))
  else
    printf 'FAIL %s\n  expected to contain: %s\n  actual: %s\n' "$name" "$needle" "$haystack"
    fail=$((fail + 1))
  fi
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    printf 'ok   %s\n' "$name"
    pass=$((pass + 1))
  else
    printf 'FAIL %s\n  expected NOT to contain: %s\n  actual: %s\n' "$name" "$needle" "$haystack"
    fail=$((fail + 1))
  fi
}

unique_sid() { printf 'e2e-%s-%s-%s\n' "$$" "$RANDOM" "$(date +%s%N 2>/dev/null || date +%s)"; }

# ---------------------------------------------------------------------------
# Build a real repo with two files spanned by one mesh, with a real mesh
# commit on the mesh ref.
# ---------------------------------------------------------------------------
cd "$WORK"
git init -q
git config user.email test@example.com
git config user.name test
printf 'a\nb\nc\nd\ne\n' > api.ts
printf 'x\ny\nz\nw\n' > api.test.ts
git add .
git commit -qm init

git-mesh add api-contract 'api.ts#L1-L3' 'api.test.ts#L1-L3' >/dev/null
git-mesh why api-contract -m "API charge contract is covered by its test." >/dev/null
git-mesh commit api-contract >/dev/null

# ---------------------------------------------------------------------------
# post-tool-use.sh: standalone. Editing a mesh-covered file should emit a
# Related: block that tags the changed range with its real porcelain status.
# ---------------------------------------------------------------------------
printf 'A\nB\nc\nd\ne\n' > api.ts
post_out="$(
  printf '%s\n' '{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"'"$WORK"'/api.ts"}}' \
    | "$POST_TOOL"
)"
post_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$post_out")"
assert_not_contains "post-tool-use has no Related: header"  "$post_ctx" "Related:"
check "post-tool-use starts with the mesh name" '[[ "$post_ctx" == "api-contract mesh:"* ]]'
assert_contains "post-tool-use names the mesh with its why" "$post_ctx" "api-contract mesh: API charge contract is covered by its test."
assert_contains "post-tool-use tags changed range CHANGED"  "$post_ctx" "api.ts#L1-L3 [CHANGED]"
assert_contains "post-tool-use lists untagged partner range" "$post_ctx" "- api.test.ts#L1-L3"
assert_not_contains "post-tool-use partner has no status tag" "$post_ctx" "api.test.ts#L1-L3 ["

# Non-edit tools are a no-op.
noop_out="$(printf '%s\n' '{"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":"'"$WORK"'/api.ts"}}' | "$POST_TOOL")"
check "post-tool-use ignores non-edit tools" '[[ -z "$noop_out" ]]'

# ---------------------------------------------------------------------------
# user-prompt-submit.sh: standalone. Mentioning a mesh-covered path in the
# prompt should surface the same Related context.
# ---------------------------------------------------------------------------
prompt_out="$(
  printf '%s\n' '{"hook_event_name":"UserPromptSubmit","prompt":"please look at api.ts"}' | "$PROMPT"
)"
prompt_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$prompt_out")"
assert_not_contains "prompt-submit has no Related: header" "$prompt_ctx" "Related:"
check "prompt-submit starts with the mesh name" '[[ "$prompt_ctx" == "api-contract mesh:"* ]]'
assert_contains "prompt-submit surfaces the mesh"          "$prompt_ctx" "api-contract mesh:"
assert_contains "prompt-submit tags CHANGED range"   "$prompt_ctx" "api.ts#L1-L3 [CHANGED]"

# Prompts with no path reference produce nothing.
empty_prompt_out="$(printf '%s\n' '{"hook_event_name":"UserPromptSubmit","prompt":"hello"}' | "$PROMPT")"
check "prompt-submit silent without paths" '[[ -z "$empty_prompt_out" ]]'

# ---------------------------------------------------------------------------
# session-start.sh + stop.sh together. The combo should report only ranges
# that became non-fresh *during* the session, with the real porcelain status.
# ---------------------------------------------------------------------------

# Case A: session starts with drift already present -> Stop should say nothing
# (nothing newly became stale).
sid_a="$(unique_sid)"
start_in_a='{"session_id":"'"$sid_a"'","hook_event_name":"SessionStart","source":"startup"}'
stop_in_a='{"session_id":"'"$sid_a"'","hook_event_name":"Stop","stop_hook_active":false}'

printf '%s\n' "$start_in_a" | "$SESSION_START"
check "session-start writes baseline file"    '[[ -f "$HOOK_CACHE/$sid_a.txt" ]]'
baseline_a="$(cat "$HOOK_CACHE/$sid_a.txt")"
assert_contains "baseline captures pre-existing drift" "$baseline_a" "CHANGED	W	api-contract	api.ts"

stop_a_out="$(printf '%s\n' "$stop_in_a" | "$STOP")"
check "stop is silent when no new drift appeared during session" '[[ -z "$stop_a_out" ]]'

# Case B: clean session start, then drift is introduced, then Stop reports it.
# Reset the working tree so the baseline is FRESH.
printf 'a\nb\nc\nd\ne\n' > api.ts
fresh_check="$(git-mesh stale --format=porcelain --no-exit-code)"
# Baseline for B should contain only the porcelain header (no findings).
sid_b="$(unique_sid)"
start_in_b='{"session_id":"'"$sid_b"'","hook_event_name":"SessionStart","source":"startup"}'
stop_in_b='{"session_id":"'"$sid_b"'","hook_event_name":"Stop","stop_hook_active":false}'
printf '%s\n' "$start_in_b" | "$SESSION_START"
baseline_b="$(cat "$HOOK_CACHE/$sid_b.txt")"
assert_not_contains "clean-session baseline has no CHANGED entries" "$baseline_b" "CHANGED"

# Introduce drift mid-session.
printf 'A\nB\nc\nd\ne\n' > api.ts
stop_b_out="$(printf '%s\n' "$stop_in_b" | "$STOP")"
check "stop emits JSON when drift appeared during session" '[[ -n "$stop_b_out" ]]'
stop_b_sys="$(jq -r '.systemMessage' <<<"$stop_b_out")"
stop_b_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$stop_b_out")"
check "stop systemMessage matches additionalContext" '[[ "$stop_b_sys" == "$stop_b_ctx" ]]'
assert_not_contains "stop has no 'relationships that became stale' header" "$stop_b_ctx" "relationships that became stale"
check "stop output starts with the mesh name" '[[ "$stop_b_ctx" == "api-contract mesh:"* ]]'
assert_contains "stop includes mesh why" "$stop_b_ctx" "api-contract mesh: API charge contract is covered by its test."
# The key assertion for the bug fix: the tag is the real status, not a
# hard-coded [STALE].
assert_contains "stop tags range with real porcelain status" "$stop_b_ctx" "api.ts#L1-L3 [CHANGED]"
assert_not_contains "stop does NOT hard-code [STALE]" "$stop_b_ctx" "[STALE]"
# Fresh partner range appears unflagged.
assert_contains "stop lists fresh partner range untagged" "$stop_b_ctx" "- api.test.ts#L1-L3"
assert_not_contains "fresh partner has no status tag" "$stop_b_ctx" "api.test.ts#L1-L3 ["

# Case C: stop is silent when the working tree has no drift at all.
printf 'a\nb\nc\nd\ne\n' > api.ts
sid_c="$(unique_sid)"
stop_in_c='{"session_id":"'"$sid_c"'","hook_event_name":"Stop","stop_hook_active":false}'
stop_c_out="$(printf '%s\n' "$stop_in_c" | "$STOP")"
check "stop is silent when repo is fully fresh" '[[ -z "$stop_c_out" ]]'

# Cleanup cache files we created (best effort; cache dir is shared).
rm -f "$HOOK_CACHE/$sid_a.txt" "$HOOK_CACHE/$sid_b.txt" "$HOOK_CACHE/$sid_c.txt" 2>/dev/null || true

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
