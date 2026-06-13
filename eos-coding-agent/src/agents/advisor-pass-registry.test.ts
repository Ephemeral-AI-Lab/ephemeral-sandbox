import type { AgentRunId, JsonObject, ToolCallFacts, ToolUseId } from "eos-agent-sdk";
import { describe, expect, it } from "vitest";

import { requireAdvisoryPass } from "../tools/agent/ask-advisor.js";
import { AdvisorPassRegistry } from "./advisor-pass-registry.js";

const RUN = "run-1" as AgentRunId;

function facts(input: JsonObject): ToolCallFacts {
  return {
    runId: RUN,
    toolUseId: "tu-1" as ToolUseId,
    toolName: "submit_main_outcome",
    input,
  };
}

async function decide(gate: ReturnType<typeof requireAdvisoryPass>, call: ToolCallFacts) {
  if (gate.event !== "preToolUse") throw new Error("expected a preToolUse gate");
  return gate.run(call);
}

describe("advisor pass registry and gate", () => {
  it("denies a terminal submission until the exact submission has passed", async () => {
    const passes = new AdvisorPassRegistry();
    const gate = requireAdvisoryPass({ toolName: "submit_main_outcome", passes });

    expect(await decide(gate, facts({ summary: "done" }))).toMatchObject({ decision: "deny" });

    passes.recordPass(RUN, { tool_name: "submit_main_outcome", payload: { summary: "done" } });
    expect(await decide(gate, facts({ summary: "done" }))).toEqual({ decision: "passthrough" });
  });

  it("matches by canonical payload, not key order, and rejects a different payload", async () => {
    const passes = new AdvisorPassRegistry();
    const gate = requireAdvisoryPass({ toolName: "submit_main_outcome", passes });
    passes.recordPass(RUN, { tool_name: "submit_main_outcome", payload: { a: 1, b: 2 } });

    expect(await decide(gate, facts({ b: 2, a: 1 })), "key order is irrelevant").toEqual({
      decision: "passthrough",
    });
    expect(await decide(gate, facts({ a: 1, b: 3 })), "a different payload is not authorized").toMatchObject({
      decision: "deny",
    });
  });

  it("scopes passes per run", () => {
    const passes = new AdvisorPassRegistry();
    passes.recordPass(RUN, { tool_name: "submit_main_outcome", payload: { x: 1 } });
    expect(passes.hasPass("run-2" as AgentRunId, { tool_name: "submit_main_outcome", payload: { x: 1 } })).toBe(false);
  });
});
