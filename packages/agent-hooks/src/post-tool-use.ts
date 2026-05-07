/**
 * PostToolUse: recording-only.
 *
 * - `Read` records an anchored read row (file or `path#L<offset>-L<end>`).
 * - `Edit` / `MultiEdit` walk `tool_response.structuredPatch` to record one
 *   `touch` row per modified hunk; falls back to a whole-file `touch` when
 *   the patch is empty or contains a delete-all hunk.
 * - `Write` records a single `touch` keyed `added` for new files,
 *   `modified` otherwise.
 * - Any other matched tool (`Bash`, `mcp__.*`) runs `git mesh advice <sid>
 *   diff <tuid>` to attribute working-tree changes back to the snapshot
 *   captured at PreToolUse.
 *
 * The hook never emits stdout — suggestions surface only when a caller
 * invokes `git mesh advice <sid> flush` on demand.
 *
 * @see ./advice-common.ts
 */

import { dirname } from "node:path";
import { type PostToolUseInput, postToolUseHook, postToolUseOutput } from "@goodfoot/claude-code-hooks";
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

export function createPostToolUseHandler(executor: AdviceExecutor) {
  return (input: PostToolUseInput) => {
    const sid = input.session_id;
    if (!sid) return postToolUseOutput({});
    const cwd = input.cwd;
    const tuid = input.tool_use_id;

    switch (input.tool_name) {
      case "Read": {
        const ti = input.tool_input as { file_path?: string; offset?: number; limit?: number };
        if (!ti.file_path) return postToolUseOutput({});
        const fp = abspathAgainst(cwd, ti.file_path);
        const fileRoot = resolveRepoRoot(dirname(fp));
        if (!fileRoot) return postToolUseOutput({});

        const rel = relativeToRepo(fileRoot, fp);
        let anchor = rel;
        if (typeof ti.offset === "number" && typeof ti.limit === "number") {
          const end = ti.offset + ti.limit - 1;
          anchor = `${rel}#L${ti.offset}-L${end}`;
        }
        const args = tuid ? [anchor, tuid] : [anchor];
        executor({ repoRoot: fileRoot, sid, verb: "read", args });
        return postToolUseOutput({});
      }

      case "Edit":
      case "MultiEdit": {
        const ti = input.tool_input as { file_path?: string };
        if (!ti.file_path || !tuid) return postToolUseOutput({});
        const fp = abspathAgainst(cwd, ti.file_path);
        const root = resolveRepoRoot(dirname(fp));
        if (!root) return postToolUseOutput({});
        const rel = relativeToRepo(root, fp);

        const patch = readPatch(input);
        if (!patch || patch.length === 0) {
          executor({ repoRoot: root, sid, verb: "touch", args: [tuid, rel, "modified"] });
          return postToolUseOutput({});
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
          executor({ repoRoot: root, sid, verb: "touch", args: [tuid, anchor, "modified"] });
        }
        if (wholeFile) {
          executor({ repoRoot: root, sid, verb: "touch", args: [tuid, rel, "modified"] });
        }
        return postToolUseOutput({});
      }

      case "Write": {
        const ti = input.tool_input as { file_path?: string };
        if (!ti.file_path || !tuid) return postToolUseOutput({});
        const fp = abspathAgainst(cwd, ti.file_path);
        const root = resolveRepoRoot(dirname(fp));
        if (!root) return postToolUseOutput({});
        const rel = relativeToRepo(root, fp);

        const response = input.tool_response as { type?: string } | undefined;
        const kind = response?.type === "create" ? "added" : "modified";
        executor({ repoRoot: root, sid, verb: "touch", args: [tuid, rel, kind] });
        return postToolUseOutput({});
      }

      default: {
        if (!tuid) return postToolUseOutput({});
        const root = resolveRepoRoot(cwd);
        if (!root) return postToolUseOutput({});
        executor({ repoRoot: root, sid, verb: "diff", args: [tuid] });
        return postToolUseOutput({});
      }
    }
  };
}

export default postToolUseHook(
  { matcher: "Read|Edit|Write|MultiEdit|Bash|mcp__.*", timeout: 15_000 },
  createPostToolUseHandler(createDefaultAdviceExecutor()),
);
