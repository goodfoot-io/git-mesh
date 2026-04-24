#!/usr/bin/env bash
# SessionStart hook: snapshot the initial non-FRESH git-mesh findings for this
# Claude Code session. Stop compares against this file to report only drift that
# became non-FRESH during the session.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=plugins/git-mesh/bin/_lib.sh
source "$HERE/_lib.sh"
require_env

payload="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$payload")"
[[ -z "$session_id" ]] && exit 0

cache_dir="$(session_cache_dir)"
cache_path="$(session_cache_path "$session_id")"
tmp_path="$cache_path.$$"

mkdir -p "$cache_dir"
git mesh stale --format=porcelain --no-exit-code >"$tmp_path" 2>/dev/null || true
mv "$tmp_path" "$cache_path"
exit 0

# Example stdin:
# {"session_id":"abc123","transcript_path":"/Users/.../.claude/projects/.../00893aaf-19fa-41d2-8238-13269b9b3ca0.jsonl","cwd":"/repo","hook_event_name":"SessionStart","source":"startup","model":"claude-sonnet-4-6"}
#
# Example cache file at /tmp/git-mesh-claude-code/abc123.txt:
# # porcelain v1
# CHANGED	W	api-contract	src/api.ts	1	3	-
