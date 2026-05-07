import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AdviceExecutor, AdviceInvocation } from "../src/advice-common.js";

/**
 * Recording fake `AdviceExecutor`. Tests inspect `invocations` to assert the
 * exact `git mesh advice` arg list each hook produced.
 */
export function createRecordingExecutor(): {
  executor: AdviceExecutor;
  invocations: AdviceInvocation[];
  failNext: (error: Error) => void;
} {
  const invocations: AdviceInvocation[] = [];
  let pendingError: Error | null = null;
  const executor: AdviceExecutor = (inv) => {
    invocations.push(inv);
    if (pendingError) {
      const err = pendingError;
      pendingError = null;
      throw err;
    }
  };
  return {
    executor,
    invocations,
    failNext: (error) => {
      pendingError = error;
    },
  };
}

/**
 * Initialise an empty git repo in a fresh temp directory and return its
 * absolute path. Caller invokes `cleanup()` to remove it.
 */
export function makeTempRepo(): { root: string; cleanup: () => void } {
  const root = mkdtempSync(join(tmpdir(), "agent-hooks-"));
  execFileSync("git", ["init", "-q", root], { stdio: "ignore" });
  return {
    root,
    cleanup: () => rmSync(root, { recursive: true, force: true }),
  };
}
