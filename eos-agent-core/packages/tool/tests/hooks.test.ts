import { describe, expect, it } from "vitest";

import {
  eosAgentsPath,
  scriptedRunState,
  scriptedTool,
  writeTranscriptFixture,
} from "@eos/testkit";
import { z } from "zod";

import { defineTool } from "../src/define.js";
import type { HookCommand, HookConfigEntry, HookOutput } from "../src/hooks/protocol.js";
import { hookWarnings, runPipeline } from "./support.js";

/** A command hook spawning a shared `.eos-agents/tests/scripts` file. */
function scriptHook(name: string, timeoutMs?: number): HookCommand {
  return {
    type: "command",
    command: `"${process.execPath}" "${eosAgentsPath("tests/scripts", name)}"`,
    ...(timeoutMs !== undefined && { timeout_ms: timeoutMs }),
  };
}

function pre(matcher: string | undefined, ...hooks: HookCommand[]): HookConfigEntry {
  return { event: "PreToolUse", matcher, hooks };
}

const probeTool = (onExecute?: () => void) =>
  scriptedTool({
    name: "probe",
    execute: () => {
      onExecute?.();
      return Promise.resolve({ content: "ran" });
    },
  });

describe("hook command adapter", () => {
  it("denies via exit 2 with stderr as the model-visible reason; the call never executes (§15.11)", async () => {
    let executed = false;
    const result = await runPipeline(probeTool(() => (executed = true)), {
      entries: [pre("probe", scriptHook("deny-with-stderr.cjs"))],
    });
    expect(result).toMatchObject({ content: "blocked by script", is_error: true });
    expect(executed).toBe(false);
  });

  it("receives the full payload as JSON on stdin", async () => {
    const result = await runPipeline(probeTool(), {
      entries: [pre(undefined, scriptHook("echo-hook-payload.cjs"))],
    });
    expect(result.content).toBe("PreToolUse|probe|tu_1|main|false");
  });

  it("applies a script's updatedInput re-validated through the schema (§15.12)", async () => {
    const received: number[] = [];
    const calc = defineTool({
      name: "calc",
      description: "calc",
      input: z.object({ n: z.number() }),
      execute: (input) => {
        received.push(input.n);
        return Promise.resolve({ content: "ok" });
      },
    });
    const result = await runPipeline(calc, {
      input: { n: 1 },
      entries: [pre("calc", scriptHook("update-input.cjs"))],
    });
    expect(result.is_error).toBe(false);
    expect(received).toEqual([42]);
  });

  it("treats garbage stdout as passthrough with a warning (§15.13)", async () => {
    let executed = false;
    const result = await runPipeline(probeTool(() => (executed = true)), {
      entries: [pre("probe", scriptHook("garbage-stdout.cjs"))],
    });
    expect(executed, "non-blocking: the call still ran").toBe(true);
    expect(result.is_error).toBe(false);
    expect(hookWarnings(result)).toContain("not JSON");
  });

  it("treats schema-mismatched stdout as passthrough with a warning (§15.13)", async () => {
    const result = await runPipeline(probeTool(), {
      entries: [pre("probe", scriptHook("decision-maybe.cjs"))],
    });
    expect(result.is_error).toBe(false);
    expect(hookWarnings(result)).toContain(
      "did not match HookOutput",
    );
  });

  it("treats schema-mismatched callback output as passthrough with a warning", async () => {
    let executed = false;
    const result = await runPipeline(probeTool(() => (executed = true)), {
      entries: [
        pre("probe", {
          type: "callback",
          run: () =>
            Promise.resolve({ additionalContext: 123 } as unknown as HookOutput),
        }),
      ],
    });
    expect(executed).toBe(true);
    expect(result.is_error).toBe(false);
    expect(hookWarnings(result)).toContain("callback hook output did not match HookOutput");
  });

  it("treats other nonzero exits as passthrough with a warning, never a deny", async () => {
    let executed = false;
    const result = await runPipeline(probeTool(() => (executed = true)), {
      entries: [pre("probe", scriptHook("exit-3.cjs"))],
    });
    expect(executed).toBe(true);
    expect(result.is_error).toBe(false);
    expect(hookWarnings(result)).toContain("exited 3");
    expect(hookWarnings(result)).toContain("flaky");
  });

  it("kills a hook on its timeout and passes through with a warning", async () => {
    const result = await runPipeline(probeTool(), {
      entries: [pre("probe", scriptHook("hang.cjs", 250))],
    });
    expect(result.is_error).toBe(false);
    expect(hookWarnings(result)).toContain("aborted");
  }, 10_000);

  it("skips hooks whose matcher names a different tool", async () => {
    const result = await runPipeline(probeTool(), {
      entries: [pre("other_tool", scriptHook("deny-with-stderr.cjs"))],
    });
    expect(result.is_error).toBe(false);
    expect(result.content).toBe("ran");
  });

  it("infers state through run.transcript_path, never live objects", async () => {
    const transcriptPath = writeTranscriptFixture([
      { role: "user", note: "contains FORBIDDEN marker" },
    ]);
    const result = await runPipeline(probeTool(), {
      runState: scriptedRunState("main", { transcriptPath }),
      entries: [pre("probe", scriptHook("read-transcript-forbidden.cjs"))],
    });
    expect(result).toMatchObject({ content: "transcript said no", is_error: true });
  });

  it("folds parallel script outputs through the precedence kernel: deny wins", async () => {
    let executed = false;
    const result = await runPipeline(probeTool(() => (executed = true)), {
      entries: [
        pre("probe", scriptHook("allow.cjs"), scriptHook("deny-with-stderr.cjs")),
      ],
    });
    expect(result).toMatchObject({ content: "blocked by script", is_error: true });
    expect(executed).toBe(false);
  });
});
