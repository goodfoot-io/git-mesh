import { Logger } from "@goodfoot/claude-code-hooks";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import hook, { createSessionEndHandler } from "../src/session-end.js";
import { createRecordingExecutor, makeTempRepo } from "./helpers.js";

const logger = new Logger();

const baseInput = (overrides: Record<string, unknown> = {}) => ({
  session_id: "sess-1",
  transcript_path: "/tmp/t",
  cwd: "/tmp",
  hook_event_name: "SessionEnd" as const,
  reason: "logout" as const,
  ...overrides,
});

describe("session-end", () => {
  let repo: { root: string; cleanup: () => void };
  beforeAll(() => {
    repo = makeTempRepo();
  });
  afterAll(() => repo.cleanup());

  it("registers SessionEnd", () => {
    expect(hook.hookEventName).toBe("SessionEnd");
  });

  it("invokes the `end` verb against cwd's repo root", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createSessionEndHandler(executor);
    handler(baseInput({ cwd: repo.root }) as never);
    expect(invocations).toEqual([{ repoRoot: repo.root, sid: "sess-1", verb: "end", args: [] }]);
  });

  it("no-ops when cwd is not inside a git repo", () => {
    const { executor, invocations } = createRecordingExecutor();
    const handler = createSessionEndHandler(executor);
    handler(baseInput({ cwd: "/" }) as never);
    expect(invocations).toEqual([]);
  });

  it("default export returns silent output", async () => {
    const result = await hook(baseInput({ cwd: "/" }) as never, { logger });
    expect(result).toMatchObject({ _type: "SessionEnd" });
  });
});
