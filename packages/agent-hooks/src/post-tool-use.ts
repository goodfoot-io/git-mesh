/**
 * PostToolUse: recording-only.
 *
 * - `Read` records an anchored read row (file or `path#L<offset>-L<end>`).
 * - `Edit` / `Write` walk `tool_response.structuredPatch` to record one
 *   `touch` row per modified hunk; falls back to a whole-file `touch`
 *   when the patch is empty (new file or no-op) or contains a delete-all
 *   hunk.
 * - `Write` to a new file (`tool_response.type === "create"`) records a
 *   whole-file `touch` keyed `added`; partial overwrites and updates use
 *   per-hunk ranges keyed `modified`.
 * - Any other matched tool (`Bash`, `mcp__.*`) runs `git mesh advice <sid>
 *   diff <tuid>` to attribute working-tree changes back to the snapshot
 *   captured at PreToolUse.
 *
 * The hook never emits stdout — suggestions surface only when a caller
 * invokes `git mesh advice <sid> flush` on demand.
 *
 * @see ./advice-common.ts
 */

// `fp` is POSIX-canonical (see `abspathAgainst`); use the POSIX variant so
// `dirname` never reintroduces native separators on Windows.
import { dirname } from "node:path/posix";
import { type PostToolUseInput, postToolUseHook } from "@goodfoot/claude-code-hooks";
import {
  type AdviceExecutor,
  abspathAgainst,
  createDefaultAdviceExecutor,
  relativeToRepo,
  resolveRepoRoot,
} from "./advice-common.js";

interface PatchHunk {
  newStart?: number;
  newLines?: number;
}

function readPatch(input: PostToolUseInput): PatchHunk[] | null {
  const response = input.tool_response as { structuredPatch?: PatchHunk[] } | undefined;
  const patch = response?.structuredPatch;
  return Array.isArray(patch) ? patch : null;
}

function recordPatchTouches(
  executor: AdviceExecutor,
  repoRoot: string,
  sid: string,
  tuid: string,
  rel: string,
  patch: PatchHunk[] | null,
  kind: string,
): void {
  if (!patch || patch.length === 0) {
    executor({ repoRoot, sid, verb: "touch", args: [tuid, rel, kind] });
    return;
  }

  let wholeFile = false;
  for (const hunk of patch) {
    const newStart = hunk.newStart;
    const newLines = hunk.newLines;
    if (typeof newStart !== "number" || typeof newLines !== "number") continue;
    if (newLines === 0) {
      wholeFile = true;
      break;
    }
    const end = newStart + newLines - 1;
    const anchor = `${rel}#L${newStart}-L${end}`;
    executor({ repoRoot, sid, verb: "touch", args: [tuid, anchor, kind] });
  }
  if (wholeFile) {
    executor({ repoRoot, sid, verb: "touch", args: [tuid, rel, kind] });
  }
}

export function createPostToolUseHandler(executor: AdviceExecutor) {
  return (input: PostToolUseInput) => {
    const sid = input.session_id;
    if (!sid) return null;
    const cwd = input.cwd;
    const tuid = input.tool_use_id;

    switch (input.tool_name) {
      case "Read": {
        const ti = input.tool_input as { file_path?: string; offset?: number; limit?: number };
        if (!ti.file_path) return null;
        const fp = abspathAgainst(cwd, ti.file_path);
        const fileRoot = resolveRepoRoot(dirname(fp));
        if (!fileRoot) return null;

        const rel = relativeToRepo(fileRoot, fp);
        let anchor = rel;
        if (typeof ti.offset === "number" && typeof ti.limit === "number") {
          const end = ti.offset + ti.limit - 1;
          anchor = `${rel}#L${ti.offset}-L${end}`;
        }
        const args = tuid ? [anchor, tuid] : [anchor];
        executor({ repoRoot: fileRoot, sid, verb: "read", args });
        return null;
      }

      case "Edit": {
        const ti = input.tool_input as { file_path?: string };
        if (!ti.file_path || !tuid) return null;
        const fp = abspathAgainst(cwd, ti.file_path);
        const root = resolveRepoRoot(dirname(fp));
        if (!root) return null;
        const rel = relativeToRepo(root, fp);
        recordPatchTouches(executor, root, sid, tuid, rel, readPatch(input), "modified");
        return null;
      }

      case "Write": {
        const ti = input.tool_input as { file_path?: string };
        if (!ti.file_path || !tuid) return null;
        const fp = abspathAgainst(cwd, ti.file_path);
        const root = resolveRepoRoot(dirname(fp));
        if (!root) return null;
        const rel = relativeToRepo(root, fp);

        const response = input.tool_response as { type?: string } | undefined;
        if (response?.type === "create") {
          executor({ repoRoot: root, sid, verb: "touch", args: [tuid, rel, "added"] });
          return null;
        }
        recordPatchTouches(executor, root, sid, tuid, rel, readPatch(input), "modified");
        return null;
      }

      default: {
        if (!tuid) return null;
        const root = resolveRepoRoot(cwd);
        if (!root) return null;
        executor({ repoRoot: root, sid, verb: "diff", args: [tuid] });
        return null;
      }
    }
  };
}

export default postToolUseHook(
  { matcher: "Read|Edit|Write|Bash|mcp__.*", timeout: 15_000 },
  createPostToolUseHandler(createDefaultAdviceExecutor()),
);
