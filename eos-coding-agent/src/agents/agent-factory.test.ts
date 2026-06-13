import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { createAgentSdk } from "eos-agent-sdk";
import {
  ScriptedLlmClient,
  assistantMessage,
  complete,
  scriptedTurn,
  toolUseBlock,
  userMessage,
} from "eos-agent-sdk/testkit";
import { describe, expect, it } from "vitest";
import { z } from "zod";

import { tempProfileDir } from "@eos/testkit";

import { loadAgentProfiles } from "../config/profiles.js";
import { WorkflowHub } from "../workflows/hub.js";
import { buildAgentFactory, createAgentOutcomeFnWithAdvisory } from "./agent-factory.js";

const MainOutcome = z.object({ summary: z.string().min(1) });

const emptyHub = (): WorkflowHub => WorkflowHub.open({ workflows: [], providers: [] });
const recordsDir = (): string => mkdtempSync(join(tmpdir(), "eos-records-"));

describe("buildAgentFactory", () => {
  it("runs the advisory loop: ask_advisor pass authorizes the gated terminal submission", async () => {
    const operatorTurns = [
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("t1", "ask_advisor", {
              tool_name: "submit_main_outcome",
              payload: { summary: "shipped" },
            }),
          ),
        ),
      ]),
      scriptedTurn([
        complete(assistantMessage(toolUseBlock("t2", "submit_main_outcome", { summary: "shipped" }))),
      ]),
    ];
    const advisorTurns = [
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("a1", "submit_advisor_outcome", { verdict: "pass", reason: "looks correct" }),
          ),
        ),
      ]),
    ];
    const sdk = createAgentSdk({
      llmClients: {
        op: { client: new ScriptedLlmClient(operatorTurns), model: "m" },
        adv: { client: new ScriptedLlmClient(advisorTurns), model: "m" },
      },
    });
    const profiles = loadAgentProfiles(
      tempProfileDir(
        { name: "operator", llm_client_id: "op", terminal_tool: "submit_main_outcome", allowed_tools: [] },
        { name: "advisor", llm_client_id: "adv", terminal_tool: "submit_advisor_outcome", allowed_tools: [] },
      ),
    );
    const agents = buildAgentFactory(sdk, profiles, recordsDir(), emptyHub());

    const operator = agents.create(
      "operator",
      createAgentOutcomeFnWithAdvisory({
        name: "submit_main_outcome",
        schema: MainOutcome,
        advisoryPrompt: "Confirm the operator finished the goal.",
      }),
    );
    const outcome = await operator.start({ messages: [userMessage("ship it")] }).outcome();

    expect(outcome.status).toBe("completed");
    if (outcome.status === "completed") expect(outcome.outcome).toEqual({ summary: "shipped" });
  });

  it("rejects a profile that lists a factory-injected tool in allowed_tools", () => {
    const sdk = createAgentSdk({ llmClients: { op: { client: new ScriptedLlmClient([]), model: "m" } } });
    const profiles = loadAgentProfiles(
      tempProfileDir(
        { name: "advisor", llm_client_id: "op", terminal_tool: "submit_advisor_outcome", allowed_tools: [] },
        { name: "rogue", llm_client_id: "op", allowed_tools: ["ask_advisor"] },
      ),
    );
    const agents = buildAgentFactory(sdk, profiles, recordsDir(), emptyHub());
    expect(() => agents.create("rogue")).toThrow(/factory-injected tool "ask_advisor"/);
  });

  it("requires a configured advisor profile bound to submit_advisor_outcome", () => {
    const sdk = createAgentSdk({ llmClients: { op: { client: new ScriptedLlmClient([]), model: "m" } } });
    const profiles = loadAgentProfiles(
      tempProfileDir({ name: "operator", llm_client_id: "op", terminal_tool: "submit_main_outcome", allowed_tools: [] }),
    );
    expect(() => buildAgentFactory(sdk, profiles, recordsDir(), emptyHub())).toThrow(/"advisor" profile/);
  });
});
