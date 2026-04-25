#!/usr/bin/env bash
# End-to-end tests for the git-mesh Claude Code hooks (shim era).
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
CACHE_DIR="/tmp/git-mesh-claude-code"
mkdir -p "$CACHE_DIR"

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

pass=0
fail=0

ok() {
  printf 'ok   %s\n' "$1"
  pass=$((pass + 1))
}

ko() {
  printf 'FAIL %s\n  %s\n' "$1" "$2"
  fail=$((fail + 1))
}

assert_true() {
  local name="$1"
  shift
  if "$@"; then
    ok "$name"
  else
    ko "$name" "condition failed"
  fi
}

assert_false() {
  local name="$1"
  shift
  if ! "$@"; then
    ok "$name"
  else
    ko "$name" "expected false, got true"
  fi
}

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    ok "$name"
  else
    ko "$name" "expected to contain: $needle — actual: $haystack"
  fi
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    ok "$name"
  else
    ko "$name" "expected NOT to contain: $needle — actual: $haystack"
  fi
}

assert_empty() {
  local name="$1" value="$2"
  if [[ -z "$value" ]]; then
    ok "$name"
  else
    ko "$name" "expected empty, got: $value"
  fi
}

assert_nonempty() {
  local name="$1" value="$2"
  if [[ -n "$value" ]]; then
    ok "$name"
  else
    ko "$name" "expected non-empty, got empty"
  fi
}

assert_eq() {
  local name="$1" a="$2" b="$3"
  if [[ "$a" == "$b" ]]; then
    ok "$name"
  else
    ko "$name" "expected equal — left: $a — right: $b"
  fi
}

unique_sid() { printf 'e2e-%s-%s-%s\n' "$$" "$RANDOM" "$(date +%s%N 2>/dev/null || date +%s)"; }

# ---------------------------------------------------------------------------
# Build a real repo with two files spanned by one mesh.
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
# Test 1: session-start.sh creates a DB file at the expected path.
# ---------------------------------------------------------------------------
sid_1="$(unique_sid)"
printf '%s\n' '{"session_id":"'"$sid_1"'","hook_event_name":"SessionStart","source":"startup"}' \
  | "$SESSION_START" >/dev/null
assert_true "session-start creates DB file" test -f "$CACHE_DIR/$sid_1.db"

# ---------------------------------------------------------------------------
# Test 2: post-tool-use.sh on a meshed file emits advice with correct shape.
# ---------------------------------------------------------------------------
sid_2="$(unique_sid)"
printf 'A\nB\nc\nd\ne\n' > api.ts
post_out="$(
  printf '%s\n' '{"session_id":"'"$sid_2"'","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"'"$WORK"'/api.ts"}}' \
    | "$POST_TOOL"
)"
post_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$post_out")"
# All non-blank lines must start with '#'
non_hash_lines="$(printf '%s\n' "$post_ctx" | grep -v '^$' | grep -v '^#' || true)"
assert_empty "post-tool-use all non-blank lines start with #" "$non_hash_lines"
assert_contains "post-tool-use contains mesh name" "$post_ctx" "api-contract"
assert_contains "post-tool-use names a partner range" "$post_ctx" "api.test.ts#L1-L3"

# MultiEdit is treated like Edit: same file_path shape, same advice surfaces.
sid_2b="$(unique_sid)"
printf 'A\nB\nC\nd\ne\n' > api.ts
multi_out="$(
  printf '%s\n' '{"session_id":"'"$sid_2b"'","hook_event_name":"PostToolUse","tool_name":"MultiEdit","tool_input":{"file_path":"'"$WORK"'/api.ts","edits":[{"old_string":"A","new_string":"AA"}]}}' \
    | "$POST_TOOL"
)"
multi_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$multi_out")"
assert_contains "post-tool-use handles MultiEdit" "$multi_ctx" "api-contract"
rm -f "$CACHE_DIR/$sid_2b.db" "$CACHE_DIR/$sid_2b.jsonl" 2>/dev/null || true

# Non-edit tools are a no-op.
noop_out="$(
  printf '%s\n' '{"session_id":"'"$sid_2"'","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":"'"$WORK"'/api.ts"}}' \
    | "$POST_TOOL"
)"
assert_empty "post-tool-use ignores non-edit tools" "$noop_out"

# ---------------------------------------------------------------------------
# Test 3: user-prompt-submit.sh mentioning a meshed path emits advice.
# ---------------------------------------------------------------------------
sid_3="$(unique_sid)"
prompt_out="$(
  printf '%s\n' '{"session_id":"'"$sid_3"'","hook_event_name":"UserPromptSubmit","prompt":"please look at api.ts"}' \
    | "$PROMPT"
)"
prompt_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$prompt_out")"
non_hash_prompt="$(printf '%s\n' "$prompt_ctx" | grep -v '^$' | grep -v '^#' || true)"
assert_empty "user-prompt-submit all non-blank lines start with #" "$non_hash_prompt"
assert_contains "user-prompt-submit surfaces mesh name" "$prompt_ctx" "api-contract"
assert_contains "user-prompt-submit names a partner range" "$prompt_ctx" "api.test.ts#L1-L3"

# Prompt with no recognizable path produces nothing.
empty_out="$(
  printf '%s\n' '{"session_id":"'"$sid_3"'","hook_event_name":"UserPromptSubmit","prompt":"hello world"}' \
    | "$PROMPT"
)"
assert_empty "user-prompt-submit silent without recognizable paths" "$empty_out"

# ---------------------------------------------------------------------------
# Test 4: stop.sh after a write event emits systemMessage matching additionalContext.
# ---------------------------------------------------------------------------
sid_4="$(unique_sid)"
git mesh advice "$sid_4" add --write api.ts 2>/dev/null || true
stop_out="$(
  printf '%s\n' '{"session_id":"'"$sid_4"'","hook_event_name":"Stop","stop_hook_active":false}' \
    | "$STOP"
)"
stop_sys="$(jq -r '.systemMessage' <<<"$stop_out")"
stop_ctx="$(jq -r '.hookSpecificOutput.additionalContext' <<<"$stop_out")"
assert_eq "stop systemMessage matches additionalContext" "$stop_sys" "$stop_ctx"
assert_contains "stop contains mesh name" "$stop_ctx" "api-contract"

# ---------------------------------------------------------------------------
# Test 5: second flush for same session/trigger produces no output (dedup).
# ---------------------------------------------------------------------------
sid_5="$(unique_sid)"
git mesh advice "$sid_5" add --read api.ts 2>/dev/null || true
first_flush="$(git mesh advice "$sid_5" 2>/dev/null || true)"
second_flush="$(git mesh advice "$sid_5" 2>/dev/null || true)"
assert_nonempty "first flush has output" "$first_flush"
assert_empty "second flush is empty (dedup)" "$second_flush"

# ---------------------------------------------------------------------------
# Test 6: stop.sh with session that has no new advice exits silently.
# ---------------------------------------------------------------------------
sid_6="$(unique_sid)"
printf '%s\n' '{"session_id":"'"$sid_6"'","hook_event_name":"SessionStart","source":"startup"}' \
  | "$SESSION_START" >/dev/null || true
# Drain the initial flush so subsequent stop has nothing new.
git mesh advice "$sid_6" 2>/dev/null || true
stop_empty="$(
  printf '%s\n' '{"session_id":"'"$sid_6"'","hook_event_name":"Stop","stop_hook_active":false}' \
    | "$STOP"
)"
assert_empty "stop is silent when no new advice to surface" "$stop_empty"

# Cleanup session files we created (best effort).
for sid in "$sid_1" "$sid_2" "$sid_3" "$sid_4" "$sid_5" "$sid_6"; do
  rm -f "$CACHE_DIR/$sid.db" "$CACHE_DIR/$sid.jsonl" 2>/dev/null || true
done

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
