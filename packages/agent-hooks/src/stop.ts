/**
 * Stop: invoke `git mesh advice <sid> flush` and surface its stdout as a
 * `systemMessage`. The flush verb dedupes against `meshes-seen.jsonl` /
 * `advice-seen.jsonl`, so repeated stops only emit deltas.
 *
 * Returns `null` (exit 0, no stdout) when the CLI has nothing to say.
 *
 * @see ./advice-common.ts
 * @see packages/git-mesh/src/cli/advice/mod.rs `run_advice_flush`
 */

import { type StopInput, stopHook, stopOutput } from "@goodfoot/claude-code-hooks";
import {
  type CapturingAdviceExecutor,
  createDefaultCapturingAdviceExecutor,
  resolveRepoRoot,
} from "./advice-common.js";

export function createStopHandler(executor: CapturingAdviceExecutor) {
  return (input: StopInput) => {
    const sid = input.session_id;
    if (!sid) return null;
    const root = resolveRepoRoot(input.cwd);
    if (!root) return null;

    const output = executor({ repoRoot: root, sid, verb: "flush", args: [] }).trim();
    if (output.length === 0) return null;

    return stopOutput({ systemMessage: output });
  };
}

export default stopHook({ matcher: "*", timeout: 15_000 }, createStopHandler(createDefaultCapturingAdviceExecutor()));
