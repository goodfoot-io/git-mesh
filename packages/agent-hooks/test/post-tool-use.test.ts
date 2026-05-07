import { mkdirSync } from "node:fs";
import { Logger } from "@goodfoot/claude-code-hooks";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import hook, { createPostToolUseHandler } from "../src/post-tool-use.js";
import { createRecordingExecutor, makeTempRepo } from "./helpers.js";

const logger = new Logger();

const baseInput = (overrides: Record<string, unknown> = {}) => ({
  session_id: "sess-1",
  transcript_path: "/tmp/t",
  cwd: "/tmp",
  hook_event_name: "PostToolUse" as const,
  tool_use_id: "tu-1",
  tool_name: "Bash",
  tool_input: {},
  tool_response: {},
  ...overrides,
});

describe("post-tool-use", () => {
  let repo: { root: string; cleanup: () => void };
  beforeAll(() => {
    repo = makeTempRepo();
    mkdirSync(`${repo.root}/src`, { recursive: true });
  });
  afterAll(() => repo.cleanup());

  it("registers PostToolUse with the documented matcher", () => {
    expect(hook.hookEventName).toBe("PostToolUse");
  });

  it("records a whole-file Read anchor when offset/limit are missing", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Read",
        tool_input: { file_path: `${repo.root}/src/foo.ts` },
      }) as never,
    );
    expect(invocations).toEqual([{ repoRoot: repo.root, sid: "sess-1", verb: "read", args: ["src/foo.ts", "tu-1"] }]);
  });

  it("records a line-range Read anchor when offset/limit are present", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Read",
        tool_input: { file_path: `${repo.root}/src/foo.ts`, offset: 10, limit: 5 },
      }) as never,
    );
    expect(invocations[0]?.args).toEqual(["src/foo.ts#L10-L14", "tu-1"]);
  });

  it("Edit with structuredPatch records one touch per hunk", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Edit",
        tool_input: { file_path: `${repo.root}/a.ts` },
        tool_response: {
          structuredPatch: [
            { newStart: 5, newLines: 3 },
            { newStart: 20, newLines: 1 },
          ],
        },
      }) as never,
    );
    expect(invocations.map((i) => i.args)).toEqual([
      ["tu-1", "a.ts#L5-L7", "modified"],
      ["tu-1", "a.ts#L20-L20", "modified"],
    ]);
  });

  it("Edit with empty structuredPatch falls back to whole-file touch", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Edit",
        tool_input: { file_path: `${repo.root}/a.ts` },
        tool_response: { structuredPatch: [] },
      }) as never,
    );
    expect(invocations).toEqual([
      { repoRoot: repo.root, sid: "sess-1", verb: "touch", args: ["tu-1", "a.ts", "modified"] },
    ]);
  });

  it("Edit with a newLines=0 hunk emits a whole-file touch", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Edit",
        tool_input: { file_path: `${repo.root}/a.ts` },
        tool_response: { structuredPatch: [{ newStart: 1, newLines: 0 }] },
      }) as never,
    );
    expect(invocations).toEqual([
      { repoRoot: repo.root, sid: "sess-1", verb: "touch", args: ["tu-1", "a.ts", "modified"] },
    ]);
  });

  it("Write with create response uses kind=added", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Write",
        tool_input: { file_path: `${repo.root}/new.ts` },
        tool_response: { type: "create" },
      }) as never,
    );
    expect(invocations[0]?.args).toEqual(["tu-1", "new.ts", "added"]);
  });

  it("Write without create response uses kind=modified", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: repo.root,
        tool_name: "Write",
        tool_input: { file_path: `${repo.root}/new.ts` },
        tool_response: { type: "update" },
      }) as never,
    );
    expect(invocations[0]?.args).toEqual(["tu-1", "new.ts", "modified"]);
  });

  it("Bash routes through the diff verb against cwd's repo root", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(baseInput({ cwd: repo.root, tool_name: "Bash" }) as never);
    expect(invocations).toEqual([{ repoRoot: repo.root, sid: "sess-1", verb: "diff", args: ["tu-1"] }]);
  });

  it("no-ops when file is outside any git repo", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    handler(
      baseInput({
        cwd: "/",
        tool_name: "Read",
        tool_input: { file_path: "/etc/hosts" },
      }) as never,
    );
    expect(invocations).toEqual([]);
  });

  it("propagates executor errors", () => {
    const { executor, failNext } = createRecordingExecutor();
    const handler = createPostToolUseHandler(executor);
    failNext(new Error("git mesh advice failed"));
    expect(() =>
      handler(
        baseInput({
          cwd: repo.root,
          tool_name: "Write",
          tool_input: { file_path: `${repo.root}/x.ts` },
          tool_response: { type: "create" },
        }) as never,
      ),
    ).toThrow(/advice failed/);
  });

  it("default export returns silent output for irrelevant cwd", async () => {
    const result = await hook(baseInput({ cwd: "/" }) as never, { logger });
    expect(result).toMatchObject({ _type: "PostToolUse" });
    expect(result).toHaveProperty("stdout");
  });
});
