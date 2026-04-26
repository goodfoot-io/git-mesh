#!/usr/bin/env bash
# End-to-end test for the git-mesh advice hooks.
#
# Builds a real git repository in a fresh temp dir, anchors a real mesh
# in it, then drives each of the four hook scripts with the actual JSON
# payload Claude Code would send. Stdin and stdout are real — no mocks.
#
# Pass: every hook exits 0 and the post-edit / prompt / stop renders
# carry the partner path of the mutated mesh range. Fail: non-zero exit
# from any hook, or missing advice text where it must appear.

set -euo pipefail

BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(cd "$BIN_DIR/.." && pwd)"
export CLAUDE_PLUGIN_ROOT="$PLUGIN_ROOT"

PASS=0
FAIL=0
TMP_ROOT="$(mktemp -d -t git-mesh-hook-test.XXXXXX)"
# Pin the advice store to a known per-test directory so the test can
# locate baseline.state / reads.jsonl without recomputing the FNV-64
# repo key the CLI uses.
export GIT_MESH_ADVICE_DIR="$TMP_ROOT/advice-store"
trap 'rm -rf "$TMP_ROOT"' EXIT

# Locate the per-session store dir by globbing under GIT_MESH_ADVICE_DIR
# (one repo-key subdir per repo). Sets STORE_DIR.
locate_store() {
  local sid="$1" matches
  matches=("$GIT_MESH_ADVICE_DIR"/*/"$sid")
  STORE_DIR="${matches[0]}"
}

log()  { printf '\033[36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m  ok\033[0m   %s\n' "$*"; PASS=$((PASS + 1)); }
bad()  { printf '\033[31m  FAIL\033[0m %s\n' "$*"; FAIL=$((FAIL + 1)); }

# Run a hook with a JSON payload on stdin. Captures stdout/stderr/exit.
# Sets globals: HOOK_OUT, HOOK_ERR, HOOK_RC.
run_hook() {
  local script="$1" payload="$2"
  local out_f err_f
  out_f="$(mktemp)"; err_f="$(mktemp)"
  set +e
  printf '%s' "$payload" | bash "$script" >"$out_f" 2>"$err_f"
  HOOK_RC=$?
  set -e
  HOOK_OUT="$(cat "$out_f")"
  HOOK_ERR="$(cat "$err_f")"
  rm -f "$out_f" "$err_f"
}

assert_rc_zero() {
  local label="$1"
  if [ "$HOOK_RC" -eq 0 ]; then
    ok "$label: exit 0"
  else
    bad "$label: exit $HOOK_RC; stderr: $HOOK_ERR"
  fi
}

assert_stdout_contains() {
  local label="$1" needle="$2"
  if printf '%s' "$HOOK_OUT" | grep -qF -- "$needle"; then
    ok "$label: stdout contains \`$needle\`"
  else
    bad "$label: stdout missing \`$needle\`; got: ${HOOK_OUT:-<empty>}"
  fi
}

assert_stdout_empty() {
  local label="$1"
  if [ -z "$HOOK_OUT" ]; then
    ok "$label: stdout empty"
  else
    bad "$label: expected empty stdout, got: $HOOK_OUT"
  fi
}

assert_stdout_json_field() {
  local label="$1" jq_expr="$2" expected="$3"
  local got
  got="$(printf '%s' "$HOOK_OUT" | jq -r "$jq_expr" 2>/dev/null || true)"
  if [ "$got" = "$expected" ]; then
    ok "$label: $jq_expr == $expected"
  else
    bad "$label: $jq_expr expected $expected, got $got"
  fi
}

# Build a fresh repo with a meshed pair (a.txt <-> b.txt).
make_repo() {
  local name="$1"
  local repo="$TMP_ROOT/$name"
  mkdir -p "$repo"
  (
    cd "$repo"
    git init -q -b main
    git config user.email "test@example.com"
    git config user.name "Test"
    printf 'one\ntwo\nthree\n' > a.txt
    printf 'alpha\nbeta\ngamma\n' > b.txt
    git add a.txt b.txt
    git commit -q -m "seed"
    git mesh add demo a.txt#L1-L3 b.txt#L1-L3 >/dev/null
    git mesh why demo -m "a.txt and b.txt move in lockstep" >/dev/null
    git mesh commit demo >/dev/null
  )
  printf '%s' "$repo"
}

payload() {
  # $1=event, $2=session_id, $3=cwd, [$4..]=jq -n --arg pairs to splice in
  local event="$1" sid="$2" cwd="$3"; shift 3
  jq -nc \
    --arg event "$event" --arg sid "$sid" --arg cwd "$cwd" \
    "$@" \
    '{session_id:$sid, transcript_path:"/dev/null", cwd:$cwd, permission_mode:"default", hook_event_name:$event} + $extra'
}

# ---------------------------------------------------------------------------
# Test 1: SessionStart writes baseline.state in the per-session store.
# ---------------------------------------------------------------------------
log "Test 1: SessionStart snapshot"
REPO1="$(make_repo repo1)"
SID1="sess-one"
PAYLOAD1="$(jq -nc --arg s "$SID1" --arg c "$REPO1" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup", model:"claude"}')"
run_hook "$BIN_DIR/advice-session-start.sh" "$PAYLOAD1"
assert_rc_zero "SessionStart"
locate_store "$SID1"; BASELINE="$STORE_DIR/baseline.state"
if [ -f "$BASELINE" ]; then
  ok "SessionStart: baseline.state created at $BASELINE"
else
  bad "SessionStart: baseline.state missing at $BASELINE"
fi
assert_stdout_empty "SessionStart"

log "Test 1b: SessionStart with source=compact also snapshots (new session id)"
SID1B="sess-one-b"
PAYLOAD1B="$(jq -nc --arg s "$SID1B" --arg c "$REPO1" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"compact"}')"
run_hook "$BIN_DIR/advice-session-start.sh" "$PAYLOAD1B"
assert_rc_zero "SessionStart(compact)"
locate_store "$SID1B"; BASELINE1B="$STORE_DIR/baseline.state"
if [ -f "$BASELINE1B" ]; then
  ok "SessionStart(compact): baseline.state created at $BASELINE1B"
else
  bad "SessionStart(compact): baseline.state missing at $BASELINE1B"
fi

# ---------------------------------------------------------------------------
# Test 2: PostToolUse on Write surfaces the partner path of the meshed range.
# ---------------------------------------------------------------------------
log "Test 2: PostToolUse Write surfaces meshed partner"
# Mutate a.txt so the next render's incr_delta flags the meshed range.
echo "modified" >> "$REPO1/a.txt"
PAYLOAD2="$(jq -nc --arg s "$SID1" --arg c "$REPO1" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Write", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t1", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD2"
assert_rc_zero "PostToolUse(Write)"
assert_stdout_json_field "PostToolUse(Write)" '.hookSpecificOutput.hookEventName' "PostToolUse"
assert_stdout_contains "PostToolUse(Write)" "b.txt"
assert_stdout_contains "PostToolUse(Write)" "a.txt and b.txt move in lockstep"

# ---------------------------------------------------------------------------
# Test 3: PostToolUse on Read with offset/limit records a ranged read.
# ---------------------------------------------------------------------------
log "Test 3: PostToolUse Read records range in reads.jsonl"
REPO3="$(make_repo repo3)"
SID3="sess-three"
run_hook "$BIN_DIR/advice-session-start.sh" \
  "$(jq -nc --arg s "$SID3" --arg c "$REPO3" \
    '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup"}')"
assert_rc_zero "SessionStart(repo3)"

PAYLOAD3="$(jq -nc --arg s "$SID3" --arg c "$REPO3" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Read", tool_input:{file_path:"b.txt", offset:1, limit:3}, tool_response:{}, tool_use_id:"t2", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD3"
assert_rc_zero "PostToolUse(Read)"
locate_store "$SID3"; READS="$STORE_DIR/reads.jsonl"
if [ -f "$READS" ] && jq -e 'select(.path=="b.txt" and .start_line==1 and .end_line==3)' "$READS" >/dev/null; then
  ok "PostToolUse(Read): b.txt#L1-L3 recorded in reads.jsonl"
else
  bad "PostToolUse(Read): expected ranged read in $READS; got: $(cat "$READS" 2>/dev/null || echo MISSING)"
fi
# Reading a meshed range should produce advice surfacing its partner.
assert_stdout_contains "PostToolUse(Read)" "a.txt"

# ---------------------------------------------------------------------------
# Test 4: PostToolUse on a non-matching tool exits 0 silent.
# ---------------------------------------------------------------------------
log "Test 4: PostToolUse on Glob is a no-op"
PAYLOAD4="$(jq -nc --arg s "$SID3" --arg c "$REPO3" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Glob", tool_input:{pattern:"*.txt"}, tool_response:{}, tool_use_id:"t3", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD4"
assert_rc_zero "PostToolUse(Glob)"
assert_stdout_empty "PostToolUse(Glob)"

# ---------------------------------------------------------------------------
# Test 5: UserPromptSubmit records new path mentions and renders advice.
# ---------------------------------------------------------------------------
log "Test 5: UserPromptSubmit picks up unread paths from prompt"
REPO5="$(make_repo repo5)"
SID5="sess-five"
run_hook "$BIN_DIR/advice-session-start.sh" \
  "$(jq -nc --arg s "$SID5" --arg c "$REPO5" \
    '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup"}')"

# Prompt mentions a.txt (which exists in the repo's worktree).
PAYLOAD5="$(jq -nc --arg s "$SID5" --arg c "$REPO5" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"UserPromptSubmit", prompt:"Please look at a.txt and tell me what it does."}')"
run_hook "$BIN_DIR/advice-user-prompt.sh" "$PAYLOAD5"
assert_rc_zero "UserPromptSubmit"
locate_store "$SID5"; READS5="$STORE_DIR/reads.jsonl"
if [ -f "$READS5" ] && jq -e 'select(.path=="a.txt")' "$READS5" >/dev/null; then
  ok "UserPromptSubmit: a.txt recorded in reads.jsonl"
else
  bad "UserPromptSubmit: a.txt not recorded; reads.jsonl: $(cat "$READS5" 2>/dev/null || echo MISSING)"
fi
assert_stdout_json_field "UserPromptSubmit" '.hookSpecificOutput.hookEventName' "UserPromptSubmit"
# Mentioning a.txt should surface its meshed partner b.txt.
assert_stdout_contains "UserPromptSubmit" "b.txt"

# ---------------------------------------------------------------------------
# Test 6: UserPromptSubmit outside a git repo is a silent no-op.
# ---------------------------------------------------------------------------
log "Test 6: UserPromptSubmit outside a git repo is silent"
NONREPO="$TMP_ROOT/non-repo"; mkdir -p "$NONREPO"
PAYLOAD6="$(jq -nc --arg c "$NONREPO" \
  '{session_id:"x", transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"UserPromptSubmit", prompt:"foo.txt"}')"
run_hook "$BIN_DIR/advice-user-prompt.sh" "$PAYLOAD6"
assert_rc_zero "UserPromptSubmit(non-repo)"
assert_stdout_empty "UserPromptSubmit(non-repo)"

# ---------------------------------------------------------------------------
# Test 7: Stop hook flushes; skipped on max_tokens.
# ---------------------------------------------------------------------------
log "Test 7: Stop hook"
# Make a fresh post-edit crossing in repo5 so Stop has something new to render.
echo "more" >> "$REPO5/b.txt"
PAYLOAD7="$(jq -nc --arg s "$SID5" --arg c "$REPO5" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"Stop", stop_reason:"end_turn", output:""}')"
run_hook "$BIN_DIR/advice-stop.sh" "$PAYLOAD7"
assert_rc_zero "Stop(end_turn)"
assert_stdout_contains "Stop(end_turn)" "a.txt"

PAYLOAD7B="$(jq -nc --arg s "$SID5" --arg c "$REPO5" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"Stop", stop_reason:"max_tokens", output:""}')"
run_hook "$BIN_DIR/advice-stop.sh" "$PAYLOAD7B"
assert_rc_zero "Stop(max_tokens)"
assert_stdout_empty "Stop(max_tokens)"

# ---------------------------------------------------------------------------
# Test 8: Hooks fail-open when no baseline exists yet.
# ---------------------------------------------------------------------------
log "Test 8: PostToolUse with no baseline is a silent no-op"
REPO8="$(make_repo repo8)"
PAYLOAD8="$(jq -nc --arg c "$REPO8" \
  '{session_id:"never-snapshotted", transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Write", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t8", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD8"
assert_rc_zero "PostToolUse(no baseline)"
assert_stdout_empty "PostToolUse(no baseline)"

# ---------------------------------------------------------------------------
# Test 9: PostToolUse Write resolves the repo from the file path even
# when cwd points at a different repo.
# ---------------------------------------------------------------------------
log "Test 9: PostToolUse Write resolves repo from tool target"
REPO9A="$(make_repo repo9a)"   # cwd repo, no advice expected
REPO9B="$(make_repo repo9b)"   # target repo (separate meshed pair)
SID9="sess-nine"
# Snapshot the target repo (the file's repo), not cwd's.
run_hook "$BIN_DIR/advice-session-start.sh" \
  "$(jq -nc --arg s "$SID9" --arg c "$REPO9B" \
    '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup"}')"
echo "edited" >> "$REPO9B/a.txt"
PAYLOAD9="$(jq -nc --arg s "$SID9" --arg c "$REPO9A" --arg fp "$REPO9B/a.txt" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Write", tool_input:{file_path:$fp}, tool_response:{}, tool_use_id:"t9", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD9"
assert_rc_zero "PostToolUse(cross-repo Write)"
assert_stdout_contains "PostToolUse(cross-repo Write)" "b.txt"
assert_stdout_contains "PostToolUse(cross-repo Write)" "a.txt and b.txt move in lockstep"

# ---------------------------------------------------------------------------
# Test 10: PostToolUse Bash with `cd /other-repo && …` resolves to that
# repo's advice store.
# ---------------------------------------------------------------------------
log "Test 10: PostToolUse Bash parses cd into a separate repo"
REPO10A="$(make_repo repo10a)"
REPO10B="$(make_repo repo10b)"
SID10="sess-ten"
run_hook "$BIN_DIR/advice-session-start.sh" \
  "$(jq -nc --arg s "$SID10" --arg c "$REPO10B" \
    '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup"}')"
echo "bash-edit" >> "$REPO10B/a.txt"
CMD10="cd $REPO10B && echo done"
PAYLOAD10="$(jq -nc --arg s "$SID10" --arg c "$REPO10A" --arg cmd "$CMD10" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Bash", tool_input:{command:$cmd}, tool_response:{}, tool_use_id:"t10", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD10"
assert_rc_zero "PostToolUse(Bash cd)"
assert_stdout_contains "PostToolUse(Bash cd)" "b.txt"

# ---------------------------------------------------------------------------
# Test 11: PostToolUse Bash with `git -C /other-repo …` resolves the
# target repo even without a cd.
# ---------------------------------------------------------------------------
log "Test 11: PostToolUse Bash parses git -C target"
REPO11A="$(make_repo repo11a)"
REPO11B="$(make_repo repo11b)"
SID11="sess-eleven"
run_hook "$BIN_DIR/advice-session-start.sh" \
  "$(jq -nc --arg s "$SID11" --arg c "$REPO11B" \
    '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionStart", source:"startup"}')"
echo "via-git-C" >> "$REPO11B/a.txt"
CMD11="git -C $REPO11B status"
PAYLOAD11="$(jq -nc --arg s "$SID11" --arg c "$REPO11A" --arg cmd "$CMD11" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Bash", tool_input:{command:$cmd}, tool_response:{}, tool_use_id:"t11", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD11"
assert_rc_zero "PostToolUse(Bash git -C)"
assert_stdout_contains "PostToolUse(Bash git -C)" "b.txt"

# ---------------------------------------------------------------------------
log ""
log "Summary: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
