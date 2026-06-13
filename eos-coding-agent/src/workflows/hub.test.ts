import { defineTool } from "eos-agent-sdk";
import { describe, expect, it } from "vitest";
import { z } from "zod";

import type { AgentFactory } from "../agents/agent-factory.js";
import type { WorkflowConfig, WorkflowProvider } from "./contract.js";
import { WorkflowHub } from "./hub.js";

const UNUSED_FACTORY = { create: () => { throw new Error("unused"); } } as unknown as AgentFactory;

function demoProvider(): WorkflowProvider {
  return {
    type: "demo",
    args: z.record(z.string(), z.unknown()),
    create: (name) => () =>
      [defineTool({ name: `do_${name}`, description: "d", input: z.object({}), execute: () => Promise.resolve({ output: "ok" }) })],
  };
}

function config(over: Partial<WorkflowConfig> = {}): WorkflowConfig {
  return { name: "wf", type: "demo", args: {}, description: "Demo workflow.", docs: "the manual", tools: ["do_wf"], ...over };
}

describe("WorkflowHub", () => {
  it("opens configured workflows and serves a profile-scoped view", () => {
    const hub = WorkflowHub.open({ workflows: [config()], providers: [demoProvider()] });
    const view = hub.forProfile(["wf"]);
    expect(view.names()).toEqual(["wf"]);
    expect(view.docs("wf")).toBe("the manual");
    expect(view.promptFragment()).toContain("do_wf");
    expect(view.tools(UNUSED_FACTORY).map((tool) => tool.name)).toEqual(["do_wf"]);
    expect([...hub.declaredToolNames()]).toEqual(["do_wf"]);
  });

  it("fails fast on an unknown workflow type", () => {
    expect(() => WorkflowHub.open({ workflows: [config({ type: "missing" })], providers: [demoProvider()] })).toThrow(
      /unknown type "missing"/,
    );
  });

  it("asserts produced tool names equal the declared frontmatter tools", () => {
    const hub = WorkflowHub.open({
      workflows: [config({ tools: ["wrong_name"] })],
      providers: [demoProvider()],
    });
    expect(() => hub.forProfile(["wf"]).tools(UNUSED_FACTORY)).toThrow(/did not produce/);
  });

  it("hides workflows a profile does not list", () => {
    const hub = WorkflowHub.open({ workflows: [config()], providers: [demoProvider()] });
    expect(() => hub.forProfile(["other"])).toThrow(/unknown workflow "other"/);
  });
});
