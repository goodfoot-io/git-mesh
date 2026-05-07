import { Logger } from "@goodfoot/claude-code-hooks";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import hook, { createStopHandler } from "../src/stop.js";
import { createCapturingExecutor, makeTempRepo } from "./helpers.js";

const logger = new Logger();

const baseInput = (overrides: Record<string, unknown> = {}) => ({
  session_id: "sess-1",
  transcript_path: "/tmp/t",
  cwd: "/tmp",
  hook_event_name: "Stop" as const,
  stop_hook_active: false,
  ...overrides,
});

describe("stop", () => {
  let repo: { root: string; cleanup: () => void };
  beforeAll(() => {
    repo = makeTempRepo();
  });
  afterAll(() => repo.cleanup());

  it("registers Stop", () => {
    expect(hook.hookEventName).toBe("Stop");
  });

  it("invokes the `flush` verb against cwd's repo root", () => {
    const { executor, invocations } = createCapturingExecutor("");
    const handler = createStopHandler(executor);
    handler(baseInput({ cwd: repo.root }) as never);
    expect(invocations).toEqual([{ repoRoot: repo.root, sid: "sess-1", verb: "flush", args: [] }]);
  });

  it("returns null when flush produces no output", () => {
    const { executor } = createCapturingExecutor("   \n  ");
    const handler = createStopHandler(executor);
    const result = handler(baseInput({ cwd: repo.root }) as never);
    expect(result).toBeNull();
  });

  it("includes flush stdout as systemMessage when non-empty", () => {
    const advice = "# Mesh advice for session `sess-1`\n\nthings to know";
    const { executor } = createCapturingExecutor(advice);
    const handler = createStopHandler(executor);
    const result = handler(baseInput({ cwd: repo.root }) as never);
    expect(result).toMatchObject({ _type: "Stop" });
    const stdout = (result as { stdout: { systemMessage?: string } }).stdout;
    expect(stdout.systemMessage).toBe(advice);
  });

  it("returns null when cwd is not inside a git repo", () => {
    const { executor, invocations } = createCapturingExecutor("");
    const handler = createStopHandler(executor);
    const result = handler(baseInput({ cwd: "/" }) as never);
    expect(result).toBeNull();
    expect(invocations).toEqual([]);
  });

  it("default export tolerates non-repo cwd", async () => {
    const result = await hook(baseInput({ cwd: "/" }) as never, { logger });
    expect(result).toBeNull();
  });
});
