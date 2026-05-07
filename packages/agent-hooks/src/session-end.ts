/**
 * SessionEnd: invoke `git mesh advice <sid> end` so the CLI can clear the
 * session advice directory and any leftover snapshot pairs.
 *
 * @see ./advice-common.ts
 */

import { type SessionEndInput, sessionEndHook, sessionEndOutput } from "@goodfoot/claude-code-hooks";
import { type AdviceExecutor, createDefaultAdviceExecutor, resolveRepoRoot } from "./advice-common.js";

export function createSessionEndHandler(executor: AdviceExecutor) {
  return (input: SessionEndInput) => {
    const sid = input.session_id;
    if (!sid) return sessionEndOutput({});
    const root = resolveRepoRoot(input.cwd);
    if (!root) return sessionEndOutput({});

    executor({ repoRoot: root, sid, verb: "end", args: [] });
    return sessionEndOutput({});
  };
}

export default sessionEndHook(
  { matcher: "*", timeout: 15_000 },
  createSessionEndHandler(createDefaultAdviceExecutor()),
);
