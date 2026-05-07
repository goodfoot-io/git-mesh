import { Logger } from "@goodfoot/claude-code-hooks";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import hook, { createPreToolUseHandler } from "../src/pre-tool-use.js";
import { createRecordingExecutor, makeTempRepo } from "./helpers.js";

const logger = new Logger();

const baseInput = (overrides: Record<string, unknown> = {}) => ({
  session_id: "sess-1",
  transcript_path: "/tmp/t",
  cwd: "/tmp",
  hook_event_name: "PreToolUse" as const,
  tool_use_id: "tu-1",
  tool_name: "Bash",
  tool_input: { command: "ls" },
  ...overrides,
});

describe("pre-tool-use", () => {
  let repo: { root: string; cleanup: () => void };
  beforeAll(() => {
    repo = makeTempRepo();
  });
  afterAll(() => repo.cleanup());

  it("registers PreToolUse with the documented matcher", () => {
    expect(hook.hookEventName).toBe("PreToolUse");
  });

  it("records `mark <tuid>` for non-skipped tools inside a git repo", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPreToolUseHandler(executor);
    const input = baseInput({ cwd: repo.root, tool_name: "Bash" });
    const out = handler(input as never);
    expect(invocations).toEqual([{ repoRoot: repo.root, sid: "sess-1", verb: "mark", args: ["tu-1"] }]);
    expect(out).toMatchObject({ _type: "PreToolUse" });
  });

  it.each([
    "Read",
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "Edit",
    "Write",
    "MultiEdit",
  ])("no-ops for skipped tool %s", (tool) => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPreToolUseHandler(executor);
    handler(baseInput({ cwd: repo.root, tool_name: tool }) as never);
    expect(invocations).toEqual([]);
  });

  it("no-ops when cwd is not inside a git repo", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPreToolUseHandler(executor);
    handler(baseInput({ cwd: "/" }) as never);
    expect(invocations).toEqual([]);
  });

  it("no-ops when tool_use_id is missing", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createPreToolUseHandler(executor);
    handler(baseInput({ cwd: repo.root, tool_use_id: "" }) as never);
    expect(invocations).toEqual([]);
  });

  it("propagates executor errors so the factory wrapper can log them", async () => {
    const { executor, failNext } = createRecordingExecutor();
    const handler = createPreToolUseHandler(executor);
    failNext(new Error("git mesh advice failed"));
    expect(() => handler(baseInput({ cwd: repo.root }) as never)).toThrow(/advice failed/);
  });

  it("default export wraps a handler that returns silent output", async () => {
    const result = await hook(baseInput({ cwd: "/" }) as never, { logger });
    expect(result).toMatchObject({ _type: "PreToolUse" });
    expect(result).toHaveProperty("stdout");
  });
});
