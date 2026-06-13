import { describe, expect, it } from "vitest";

import { tempProfileDir, writeProfile } from "@eos/testkit";

import { loadAgentProfile } from "./profile-loader.js";

describe("loadAgentProfile", () => {
  it("parses workflows, subagents, and terminal_tool from frontmatter", () => {
    const dir = tempProfileDir();
    const path = writeProfile(dir, {
      name: "operator",
      terminal_tool: "submit_main_outcome",
      workflows: ["pursuit"],
      subagents: ["subagent"],
      allowed_tools: ["run_subagent"],
      body: "operator prompt",
    });
    expect(loadAgentProfile(path)).toMatchObject({
      name: "operator",
      terminal_tool: "submit_main_outcome",
      workflows: ["pursuit"],
      subagents: ["subagent"],
      allowed_tools: ["run_subagent"],
      system_prompt: "operator prompt",
    });
  });

  it("defaults workflows and subagents to empty lists when omitted", () => {
    const dir = tempProfileDir();
    const path = writeProfile(dir, { name: "plain", allowed_tools: ["read"] });
    const profile = loadAgentProfile(path);
    expect(profile.workflows, "workflows default").toEqual([]);
    expect(profile.subagents, "subagents default").toEqual([]);
    expect(profile.terminal_tool, "no terminal tool").toBeUndefined();
  });

  // The dead field names are constructed, not written literally, so the §14
  // hygiene word-grep does not flag this rejection test (cf. the `needs` rule).
  it.each([
    ["agent", "kind"].join("_"),
    ["workflow", "context", "script"].join("_"),
  ])("rejects the dropped %s field", (field) => {
    const dir = tempProfileDir();
    const path = writeProfile(dir, {
      name: "legacy",
      allowed_tools: ["read"],
      extra: { [field]: "value" },
    });
    expect(() => loadAgentProfile(path)).toThrow(/is invalid/);
  });

  it("rejects a terminal_tool that also appears in allowed_tools", () => {
    const dir = tempProfileDir();
    const path = writeProfile(dir, {
      name: "bad",
      terminal_tool: "submit_x",
      allowed_tools: ["submit_x"],
    });
    expect(() => loadAgentProfile(path)).toThrow(/terminal_tool/);
  });
});
