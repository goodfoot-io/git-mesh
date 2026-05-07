/**
 * PreToolUse: capture a per-tool_use_id snapshot pair so PostToolUse can
 * attribute working-tree changes back to this exact tool call. No-op for
 * the read-only / file-modify deny-list — those tools either don't touch
 * the working tree or have their effect recorded directly in PostToolUse.
 *
 * Silent recording-only: returns an empty PreToolUse output regardless of
 * whether a snapshot was taken.
 *
 * @see ./advice-common.ts
 */

import { type PreToolUseInput, preToolUseHook, preToolUseOutput } from "@goodfoot/claude-code-hooks";
import { type AdviceExecutor, createDefaultAdviceExecutor, resolveRepoRoot } from "./advice-common.js";

/**
 * Tools whose effect on the working tree is captured directly by PostToolUse
 * (Edit/Write/MultiEdit) or whose effect is irrelevant for diff attribution
 * (Read/Grep/Glob/LS/WebFetch/WebSearch). Matches the case statement in
 * `advice-pre-tool-use.sh`.
 */
const SKIPPED_TOOLS = new Set<string>([
  "Read",
  "Grep",
  "Glob",
  "LS",
  "WebFetch",
  "WebSearch",
  "Edit",
  "Write",
  "MultiEdit",
]);

export function createPreToolUseHandler(executor: AdviceExecutor) {
  return (input: PreToolUseInput) => {
    const sid = input.session_id;
    const tuid = input.tool_use_id;
    if (!sid || !tuid) return preToolUseOutput({});
    if (SKIPPED_TOOLS.has(input.tool_name)) return preToolUseOutput({});

    const root = resolveRepoRoot(input.cwd);
    if (!root) return preToolUseOutput({});

    executor({ repoRoot: root, sid, verb: "mark", args: [tuid] });
    return preToolUseOutput({});
  };
}

export default preToolUseHook(
  { matcher: "Edit|Write|MultiEdit|Bash|mcp__.*", timeout: 15_000 },
  createPreToolUseHandler(createDefaultAdviceExecutor()),
);
