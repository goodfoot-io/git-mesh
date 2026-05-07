/**
 * Shared helpers for git-mesh advice hooks.
 *
 * Mirrors the contract of `plugins/git-mesh/bin/advice-common.sh`:
 * - resolve a directory to its containing git repo root
 * - resolve a possibly-relative path against a base
 * - shell out to `git mesh advice <sid> <verb> [args...]` with stdout discarded
 *
 * The executor is expressed as an interface so tests can supply a recording
 * fake without mocking the framework.
 */

import { type ExecFileSyncOptions, execFileSync } from "node:child_process";
import { isAbsolute, join } from "node:path";

/**
 * Captures one invocation of `git mesh advice <sid> <verb> [args...]` against
 * a specific repo root.
 */
export interface AdviceInvocation {
  repoRoot: string;
  sid: string;
  verb: string;
  args: string[];
}

/**
 * Runs a single `git mesh advice` invocation. Implementations must:
 * - run the command with `cwd === repoRoot`
 * - discard stdout (silent recording-only contract)
 * - throw on non-zero exit so the hook factory's error wrapper logs it
 */
export type AdviceExecutor = (invocation: AdviceInvocation) => void;

/**
 * Default 15s ceiling. Mirrors the `timeout: 15` setting in the original
 * `hooks.json`. The hook handler also receives the `timeout` from the
 * factory; this guards individual subprocess hangs.
 */
const ADVICE_TIMEOUT_MS = 15_000;

export function createDefaultAdviceExecutor(timeoutMs: number = ADVICE_TIMEOUT_MS): AdviceExecutor {
  return ({ repoRoot, sid, verb, args }) => {
    const opts: ExecFileSyncOptions = {
      cwd: repoRoot,
      stdio: ["ignore", "ignore", "inherit"],
      timeout: timeoutMs,
    };
    execFileSync("git", ["mesh", "advice", sid, verb, ...args], opts);
  };
}

/**
 * Resolve a directory to its containing git repo toplevel. Returns null if
 * the directory does not exist or is not inside a working tree.
 */
export function resolveRepoRoot(dir: string | undefined | null): string | null {
  if (!dir) return null;
  try {
    const out = execFileSync("git", ["-C", dir, "rev-parse", "--show-toplevel"], {
      stdio: ["ignore", "pipe", "ignore"],
      encoding: "utf8",
    });
    const trimmed = out.trim();
    return trimmed.length > 0 ? trimmed : null;
  } catch {
    return null;
  }
}

/**
 * Resolve `target` against `base` if relative, pass through if absolute.
 */
export function abspathAgainst(base: string, target: string): string {
  return isAbsolute(target) ? target : join(base, target);
}

/**
 * Compute the repo-relative path of `absPath` inside `repoRoot`. Returns
 * `absPath` unchanged if it does not start with `repoRoot/`.
 */
export function relativeToRepo(repoRoot: string, absPath: string): string {
  const prefix = repoRoot.endsWith("/") ? repoRoot : `${repoRoot}/`;
  return absPath.startsWith(prefix) ? absPath.slice(prefix.length) : absPath;
}
