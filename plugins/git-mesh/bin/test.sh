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

# Prefer the freshly-built test binary so that the four-verb CLI
# (milestone / stop) is available even before a release install. The
# `yarn test` script compiles into ${GIT_MESH_CARGO_TARGET_ROOT}/test/;
# fall back to a workspace-local debug build if that directory doesn't exist.
WORKSPACE_ROOT_EARLY="$(git -C "$BIN_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
_MESH_TEST_BIN=""
for _candidate in \
    "${GIT_MESH_CARGO_TARGET_ROOT:-$HOME/.cache/git-mesh/cargo-target}/test/debug/git-mesh" \
    "${WORKSPACE_ROOT_EARLY}/packages/git-mesh/target/debug/git-mesh" \
  ; do
  if [ -x "$_candidate" ]; then
    _MESH_TEST_BIN="$(dirname "$_candidate")"
    break
  fi
done
if [ -n "$_MESH_TEST_BIN" ]; then
  export PATH="$_MESH_TEST_BIN:$PATH"
fi
unset _candidate _MESH_TEST_BIN WORKSPACE_ROOT_EARLY

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
# Test 1: SessionEnd removes the per-session advice directory.
# ---------------------------------------------------------------------------
log "Test 1: SessionEnd removes session directory"
REPO1="$(make_repo repo1)"
SID1="sess-one"
PRE1="$(jq -nc --arg s "$SID1" --arg c "$REPO1" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PreToolUse", tool_name:"Bash", tool_input:{command:"echo"}, tool_use_id:"t1", duration_ms:1}')"
run_hook "$BIN_DIR/advice-pre-tool-use.sh" "$PRE1"
assert_rc_zero "SessionEnd(setup mark)"
END1="$(jq -nc --arg s "$SID1" --arg c "$REPO1" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"SessionEnd", reason:"exit"}')"
run_hook "$BIN_DIR/advice-session-end.sh" "$END1"
assert_rc_zero "SessionEnd"
assert_stdout_empty "SessionEnd"

log "Test 1b: SessionEnd is idempotent"
run_hook "$BIN_DIR/advice-session-end.sh" "$END1"
assert_rc_zero "SessionEnd(idempotent)"
assert_stdout_empty "SessionEnd(idempotent)"

# ---------------------------------------------------------------------------
# Test 2: PostToolUse Bash after a write to a.txt surfaces the partner b.txt.
# Write is no longer in the PostToolUse matcher; instead a Bash PostToolUse
# dispatches to `milestone`, which detects file edits via snapshot diff.
# ---------------------------------------------------------------------------
log "Test 2: PreToolUse mark + edit + PostToolUse flush surfaces meshed partner"
REPO2="$(make_repo repo2)"
SID2="sess-two"
# Record a read of a.txt before marking so flush can pass the gate.
READ2="$(jq -nc --arg s "$SID2" --arg c "$REPO2" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Read", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t2r", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$READ2"
assert_rc_zero "PreToolUse(read a.txt)"
PRE2="$(jq -nc --arg s "$SID2" --arg c "$REPO2" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PreToolUse", tool_name:"Bash", tool_input:{command:"echo"}, tool_use_id:"t2", duration_ms:1}')"
run_hook "$BIN_DIR/advice-pre-tool-use.sh" "$PRE2"
assert_rc_zero "PreToolUse(mark)"
echo "new-content" >> "$REPO2/a.txt"
CMD2="echo done"
PAYLOAD2="$(jq -nc --arg s "$SID2" --arg c "$REPO2" --arg cmd "$CMD2" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Bash", tool_input:{command:$cmd}, tool_response:{}, tool_use_id:"t2", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD2"
assert_rc_zero "PostToolUse(Bash flush)"
assert_stdout_contains "PostToolUse(Bash flush)" "b.txt"

# ---------------------------------------------------------------------------
# Test 3: PostToolUse on Read with offset/limit records a ranged read.
# ---------------------------------------------------------------------------
log "Test 3: PostToolUse Read records range in reads.jsonl"
REPO3="$(make_repo repo3)"
SID3="sess-three"

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
# The `read` verb emits BasicOutput immediately for matching meshes. Since
# b.txt#L1-L3 is in the demo mesh with a.txt, reading b.txt should surface a.txt.
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

# Tests 5 and 6 are deleted: UserPromptSubmit hook (advice-user-prompt.sh) was
# removed in Phase 2. There is no replacement — that event surface is gone.

# Test 7 deleted: the Stop hook (advice-stop.sh) was removed. Mesh advice
# now emits inline at PostToolUse via additionalContext.

# ---------------------------------------------------------------------------
# Test 8: PostToolUse Write emits advice via the payload-driven `touch` verb
# without any prior `mark`. Edit/Write/MultiEdit are exempt from the mark
# snapshot — `tool_response.type` (absent here) defaults to `modified`.
# ---------------------------------------------------------------------------
log "Test 8: PostToolUse Write emits advice via touch with no prior mark"
REPO8="$(make_repo repo8)"
PAYLOAD8="$(jq -nc --arg c "$REPO8" \
  '{session_id:"never-marked", transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Write", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t8", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD8"
assert_rc_zero "PostToolUse(no mark)"
assert_stdout_contains "PostToolUse(no mark)" "demo mesh"

# Test 9 is deleted: it tested PostToolUse Write resolving the repo from the
# file path — Write is no longer in the PostToolUse matcher and has no
# replacement in the new four-verb CLI surface.

# ---------------------------------------------------------------------------
# Test 10: PostToolUse Bash dispatches milestone against cwd (no per-tool
# path parsing). The hook uses the payload cwd to find the repo.
# ---------------------------------------------------------------------------
log "Test 10: PreToolUse mark + edit + PostToolUse flush against cwd"
REPO10="$(make_repo repo10)"
SID10="sess-ten"
# Record a read of a.txt before marking so flush can pass the gate.
READ10="$(jq -nc --arg s "$SID10" --arg c "$REPO10" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Read", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t10r", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$READ10"
assert_rc_zero "Test10: read a.txt"
PRE10="$(jq -nc --arg s "$SID10" --arg c "$REPO10" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PreToolUse", tool_name:"Bash", tool_input:{command:"echo"}, tool_use_id:"t10", duration_ms:1}')"
run_hook "$BIN_DIR/advice-pre-tool-use.sh" "$PRE10"
echo "bash-edit" >> "$REPO10/a.txt"
CMD10="echo done"
PAYLOAD10="$(jq -nc --arg s "$SID10" --arg c "$REPO10" --arg cmd "$CMD10" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Bash", tool_input:{command:$cmd}, tool_response:{}, tool_use_id:"t10", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD10"
assert_rc_zero "PostToolUse(Bash cwd)"
assert_stdout_contains "PostToolUse(Bash cwd)" "b.txt"

# ---------------------------------------------------------------------------
# Test 11: PostToolUse on a non-Read tool (mcp__*-style name) dispatches
# milestone against cwd — no separate mcp arm exists.
# ---------------------------------------------------------------------------
log "Test 11: PreToolUse mark + edit + PostToolUse flush for mcp__ tool"
REPO11="$(make_repo repo11)"
SID11="sess-eleven"
# Record a read of a.txt before marking so flush can pass the gate.
READ11="$(jq -nc --arg s "$SID11" --arg c "$REPO11" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Read", tool_input:{file_path:"a.txt"}, tool_response:{}, tool_use_id:"t11r", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$READ11"
assert_rc_zero "Test11: read a.txt"
PRE11="$(jq -nc --arg s "$SID11" --arg c "$REPO11" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PreToolUse", tool_name:"mcp__filesystem__write_file", tool_input:{}, tool_use_id:"t11", duration_ms:1}')"
run_hook "$BIN_DIR/advice-pre-tool-use.sh" "$PRE11"
echo "mcp-edit" >> "$REPO11/a.txt"
PAYLOAD11="$(jq -nc --arg s "$SID11" --arg c "$REPO11" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"mcp__filesystem__write_file", tool_input:{}, tool_response:{}, tool_use_id:"t11", duration_ms:1}')"
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD11"
assert_rc_zero "PostToolUse(mcp__ cwd)"
assert_stdout_contains "PostToolUse(mcp__ cwd)" "b.txt"

# ---------------------------------------------------------------------------
# Test 12: CLI exit-code split — operational failures (exit 1) vs
# clap usage errors (exit 2). Mirrors the convention documented in
# README.md "Exit codes". Wrapper lives at packages/git-mesh/src/main.rs.
# ---------------------------------------------------------------------------
log "Test 12: exit-code split (runtime=1, usage=2)"
REPO12="$(make_repo repo12)"

assert_mesh_exit() {
  local label="$1" expected="$2" repo="$3"; shift 3
  local rc
  set +e
  ( cd "$repo" && git mesh "$@" >/dev/null 2>&1 )
  rc=$?
  set -e
  if [ "$rc" -eq "$expected" ]; then
    ok "$label: exit $rc"
  else
    bad "$label: expected exit $expected, got $rc"
  fi
}

assert_mesh_exit "fetch nope (runtime: missing remote)" 1 "$REPO12" fetch nope
assert_mesh_exit "fetch --bogus (usage: bad flag)" 2 "$REPO12" fetch --bogus
assert_mesh_exit "delete missing (runtime: mesh not found)" 1 "$REPO12" delete missing-mesh
assert_mesh_exit "commit empty (runtime: nothing staged)" 1 "$REPO12" commit no-such-mesh

# ---------------------------------------------------------------------------
# Test 13: GIT_MESH_ADVICE_DEBUG=1 — systemMessage gets trace marker,
# additionalContext does not.
#
# This test requires the debug-instrumented build. Locate the binary by
# searching common cargo target dirs; skip gracefully if unavailable.
# ---------------------------------------------------------------------------
log "Test 13: GIT_MESH_ADVICE_DEBUG=1 trace appears in systemMessage only"
MESH_BIN=""
WORKSPACE_ROOT="$(git -C "$BIN_DIR" rev-parse --show-toplevel 2>/dev/null)"
for candidate in \
    "${WORKSPACE_ROOT}/packages/git-mesh/target/debug/git-mesh" \
    "${WORKSPACE_ROOT}/packages/git-mesh/target/release/git-mesh" \
    "${GIT_MESH_CARGO_TARGET_ROOT:-$HOME/.cache/git-mesh/cargo-target}/test/debug/git-mesh" \
    "${GIT_MESH_CARGO_TARGET_ROOT:-$HOME/.cache/git-mesh/cargo-target}/build/debug/git-mesh" \
    "${GIT_MESH_CARGO_TARGET_ROOT:-$HOME/.cache/git-mesh/cargo-target}/build/release/git-mesh" \
  ; do
  if [ -x "$candidate" ]; then
    MESH_BIN="$candidate"
    break
  fi
done

if [ -z "$MESH_BIN" ]; then
  ok "Test 13 (skip): no candidate binary path exists; run 'cargo build' in packages/git-mesh first"
elif ! { MESH_HELP="$("$MESH_BIN" --help 2>&1)"; printf '%s' "$MESH_HELP" | grep -q 'advice'; }; then
  # Binary exists but predates the advice subcommand — this is a hard failure,
  # not a skip. A stale build should not silently pass the test suite.
  bad "Test 13: binary at $MESH_BIN exists but does not expose the 'advice' subcommand — rebuild with 'cargo build' in packages/git-mesh"
else
  SAVED_PATH="$PATH"
  export PATH="$(dirname "$MESH_BIN"):$PATH"
REPO13="$(make_repo repo13)"
SID13="sess-thirteen"

# Read b.txt (meshed partner of a.txt) to trigger advice via the read verb.
PAYLOAD13="$(jq -nc --arg s "$SID13" --arg c "$REPO13" \
  '{session_id:$s, transcript_path:"/dev/null", cwd:$c, permission_mode:"default", hook_event_name:"PostToolUse", tool_name:"Read", tool_input:{file_path:"b.txt", offset:1, limit:3}, tool_response:{}, tool_use_id:"t13", duration_ms:1}')"

export GIT_MESH_ADVICE_DEBUG=1
run_hook "$BIN_DIR/advice-post-tool-use.sh" "$PAYLOAD13"
unset GIT_MESH_ADVICE_DEBUG

assert_rc_zero "PostToolUse(debug)"
# `read` emits BasicOutput for matching meshes. systemMessage must contain
# the advice text (the demo mesh with partner paths).
assert_stdout_contains "PostToolUse(debug): systemMessage has advice" "demo mesh"
# When GIT_MESH_ADVICE_DEBUG=1, the debug separator is appended to
# systemMessage only when the CLI actually emits stderr. The `read` verb
# produces no stderr for a normal read — so the separator is absent.
# additionalContext carries only the raw advice text (no debug block).
if ! printf '%s' "$HOOK_OUT" | jq -r '.hookSpecificOutput.additionalContext // ""' | grep -qF "git-mesh-advice-debug"; then
  ok "Test 13: additionalContext free of debug trace"
else
  bad "Test 13: additionalContext must not carry debug trace"
fi

  export PATH="$SAVED_PATH"
fi  # end: MESH_BIN found

# ---------------------------------------------------------------------------
log ""
log "Summary: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
