import { describe, expect, it } from "vitest";

import { BackgroundSupervisor } from "@eos/engine";
import { NotificationInbox } from "@eos/notifications";
import { scriptedRunState, scriptedTool } from "@eos/testkit";

import type { ToolDefinition } from "../src/contract.js";
import { buildToolExecutor } from "../src/toolset.js";
import {
  TERMINAL_TOOL_NAMES,
  backgroundTools,
  terminalToolDefinitions,
} from "../src/index.js";
import { live, must, toolUse } from "./support.js";

function supervisor(): BackgroundSupervisor {
  return new BackgroundSupervisor(new NotificationInbox());
}

describe("toolset assembly", () => {
  it("binds exactly the supplied definitions in sorted order (04.5 §11)", () => {
    const definitions: ToolDefinition[] = [
      scriptedTool({
        name: "zeta",
        execute: () => Promise.resolve({ content: "z" }),
      }),
      scriptedTool({
        name: "alpha",
        execute: () => Promise.resolve({ content: "a" }),
      }),
    ];
    const executor = buildToolExecutor({
      runState: scriptedRunState("worker"),
      definitions,
    });
    expect(executor.specs().map((spec) => spec.name)).toEqual(["alpha", "zeta"]);
  });

  it("never filters by agent kind: selection happened upstream in the profile", () => {
    const definitions = [
      ...backgroundTools(supervisor()),
      scriptedTool({
        name: "ask_advisor",
        execute: () => Promise.resolve({ content: "ok" }),
      }),
    ];
    const executor = buildToolExecutor({
      runState: scriptedRunState("worker"),
      definitions,
    });
    expect(executor.specs().map((spec) => spec.name)).toEqual([
      "ask_advisor",
      "cancel_background_session",
      "list_background_sessions",
    ]);
  });

  it("dispatches through the assembled pipeline: a solo submission terminates (§15.19)", async () => {
    const executor = buildToolExecutor({
      runState: scriptedRunState("main"),
      definitions: terminalToolDefinitions().filter(
        (definition) => definition.name === "submit_main_outcome",
      ),
    });
    const events: unknown[] = [];
    const results = await executor.executeBatch(
      [toolUse("tu_s", "submit_main_outcome", { summary: "shipped" })],
      live(),
      (event) => events.push(event),
    );
    expect(must(results.at(0))).toMatchObject({
      is_error: false,
      is_terminal: true,
      content: { summary: "shipped" },
    });
  });

  it("inventories one terminal definition per submission name, statically named", () => {
    const definitions = terminalToolDefinitions();
    expect(definitions.map((definition) => definition.name as string)).toEqual([
      ...TERMINAL_TOOL_NAMES,
    ]);
    expect(TERMINAL_TOOL_NAMES).toEqual([
      "submit_main_outcome",
      "submit_planner_outcome",
      "submit_worker_outcome",
      "submit_advisor_outcome",
      "submit_subagent_outcome",
    ]);
    expect(
      definitions.every((definition) => definition.isTerminal),
      "every inventory entry is terminal",
    ).toBe(true);
    expect(
      Object.fromEntries(
        definitions.map((definition) => [
          definition.name,
          {
            isAdvisoryRequired: definition.isAdvisoryRequired,
            hasAdvisorPrompt: definition.advisorPrompt !== undefined,
          },
        ]),
      ),
    ).toEqual({
      submit_main_outcome: {
        isAdvisoryRequired: true,
        hasAdvisorPrompt: true,
      },
      submit_planner_outcome: {
        isAdvisoryRequired: true,
        hasAdvisorPrompt: true,
      },
      submit_worker_outcome: {
        isAdvisoryRequired: true,
        hasAdvisorPrompt: true,
      },
      submit_advisor_outcome: {
        isAdvisoryRequired: false,
        hasAdvisorPrompt: false,
      },
      submit_subagent_outcome: {
        isAdvisoryRequired: false,
        hasAdvisorPrompt: false,
      },
    });
  });
});
