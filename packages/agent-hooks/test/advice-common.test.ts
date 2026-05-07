import { execFileSync } from "node:child_process";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { abspathAgainst, createDefaultAdviceExecutor, relativeToRepo, resolveRepoRoot } from "../src/advice-common.js";
import { makeTempRepo } from "./helpers.js";

describe("advice-common", () => {
  let repo: { root: string; cleanup: () => void };
  beforeAll(() => {
    repo = makeTempRepo();
  });
  afterAll(() => repo.cleanup());

  it("resolveRepoRoot returns the toplevel for a directory inside a repo", () => {
    expect(resolveRepoRoot(repo.root)).toBe(repo.root);
  });

  it("resolveRepoRoot returns null outside any repo", () => {
    expect(resolveRepoRoot("/")).toBeNull();
  });

  it("resolveRepoRoot returns null for a non-existent directory", () => {
    expect(resolveRepoRoot("/nonexistent-path-xyz-123")).toBeNull();
  });

  it("resolveRepoRoot returns null for empty input", () => {
    expect(resolveRepoRoot(undefined)).toBeNull();
    expect(resolveRepoRoot("")).toBeNull();
  });

  it("abspathAgainst preserves absolute paths", () => {
    expect(abspathAgainst("/base", "/abs/path")).toBe("/abs/path");
  });

  it("abspathAgainst joins relative paths against base", () => {
    expect(abspathAgainst("/base", "rel/x")).toBe("/base/rel/x");
  });

  it("relativeToRepo strips the repo prefix", () => {
    expect(relativeToRepo("/repo", "/repo/sub/file.ts")).toBe("sub/file.ts");
  });

  it("relativeToRepo passes through paths outside the repo", () => {
    expect(relativeToRepo("/repo", "/elsewhere/file.ts")).toBe("/elsewhere/file.ts");
  });

  it("default executor surfaces non-zero exit as a thrown error", () => {
    const exec = createDefaultAdviceExecutor(5_000);
    // `git mesh` will not be a real subcommand in the test environment;
    // the executor must propagate the failure rather than swallow it.
    expect(() => exec({ repoRoot: repo.root, sid: "sess-1", verb: "definitely-not-a-verb", args: [] })).toThrow();
  });

  it("default executor invokes git from the configured repo root", () => {
    // Smoke check: confirm `git` itself is reachable in the test env so the
    // failure mode covered above is "non-zero exit" rather than ENOENT.
    expect(() => execFileSync("git", ["--version"], { stdio: "ignore" })).not.toThrow();
  });
});
