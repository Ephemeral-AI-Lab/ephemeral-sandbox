import { mkdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type JsonObject } from "@eos/contracts";
import type { LlmClient, LlmRequest } from "@eos/llm-client";
import { eosAgentsPath } from "@eos/testkit";

import { createAgentRuntime, type AgentRuntime } from "../src/runtime.js";
import {
  MockLlmClient,
  assistantMessage,
  asString,
  complete,
  dynamicTurn,
  llmRegistry,
  must,
  scriptedTurn,
  tempDir,
  toolUseBlock,
  userMessage,
  writeProfile,
  type ProfileSpec,
} from "../tests/support.js";
import {
  SLEEPER_BODY,
  TERSE_BODY,
  advisoryReadyProfile,
  submissionOf,
  toolResultsIn,
  waitTool,
} from "./support/fixtures.js";

interface FixtureOptions {
  profiles: readonly ProfileSpec[];
  clients: Record<string, LlmClient>;
  baseTools?: Parameters<typeof createAgentRuntime>[0]["baseTools"];
}

function runtimeFixture(options: FixtureOptions): AgentRuntime {
  const root = tempDir("eos-submission-prehook-e2e-");
  const profilesDir = join(root, "profiles");
  mkdirSync(profilesDir, { recursive: true });
  for (const profile of options.profiles) {
    writeProfile(profilesDir, advisoryReadyProfile(profile));
  }
  return createAgentRuntime({
    agentProfilesDir: profilesDir,
    llmClients: llmRegistry(options.clients),
    baseTools: options.baseTools,
    hookConfigPath: eosAgentsPath("hooks.json"),
    notificationRulesPath: eosAgentsPath("tests/notification-rules/none.json"),
    dataDir: join(root, "data"),
  });
}

function toolResultJson(request: LlmRequest, toolUseId: string): JsonObject {
  for (const message of request.messages) {
    for (const block of message.content) {
      if (block.type === "tool_result" && block.tool_use_id === toolUseId) {
        return JSON.parse(block.content) as JsonObject;
      }
    }
  }
  throw new Error(`missing tool_result ${toolUseId}`);
}

describe("submission prehook (e2e)", () => {
  it("pre-rejects terminal submission while a background session is open, then allows submit after cancellation", async () => {
    const wait = waitTool();
    const advisorClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_advisor_submit", "submit_advisor_outcome", {
              summary: "pass",
              payload: {
                verdict: "pass",
                tool_name: "submit_main_outcome",
                payload: { summary: "cleaned up" },
                reason: "exact payload matches",
              },
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const sleeperClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(toolUseBlock("tu_wait", "wait", { ms: 60_000 })),
          "tool_use",
        ),
      ]),
    ]);
    const bossClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "sleeper",
              prompt: "hold",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_submit_early", "submit_main_outcome", {
              summary: "too early",
            }),
          ),
          "tool_use",
        ),
      ]),
      dynamicTurn((request) => {
        const spawned = toolResultJson(request, "tu_spawn");
        return [
          complete(
            assistantMessage(
              toolUseBlock("tu_cancel", "cancel_background_session", {
                type: "subagent",
                id: asString(spawned.run_id),
                reason: "clear open work before submit",
              }),
            ),
            "tool_use",
          ),
        ];
      }),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_ask_advisor", "ask_advisor", {
              tool_name: "submit_main_outcome",
              payload: { summary: "cleaned up" },
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_submit_final", "submit_main_outcome", {
              summary: "cleaned up",
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const runtime = runtimeFixture({
      profiles: [
        {
          name: "boss",
          kind: "main",
          llmClientId: "boss_llm",
          allowed: ["run_subagent", "cancel_background_session"],
          maxTurns: 8,
          body: TERSE_BODY,
        },
        {
          name: "sleeper",
          kind: "subagent",
          llmClientId: "sleeper_llm",
          allowed: ["wait"],
          maxTurns: 3,
          body: SLEEPER_BODY,
        },
        {
          name: "advisor",
          kind: "advisor",
          llmClientId: "advisor_llm",
          allowed: [],
          maxTurns: 3,
          body: TERSE_BODY,
        },
      ],
      clients: {
        boss_llm: bossClient,
        sleeper_llm: sleeperClient,
        advisor_llm: advisorClient,
      },
      baseTools: [wait.definition],
    });

    const run = runtime.startRun({
      agentName: "boss",
      initialMessages: [userMessage("verify terminal submission prehook")],
    });
    const outcome = await run.handle.outcome;

    expect(outcome.status).toBe("completed");
    expect(asString(submissionOf(outcome).summary)).toBe("cleaned up");

    const results = toolResultsIn(outcome.llm);
    const earlySubmit = must(
      results.find((result) => result.tool_use_id === "tu_submit_early"),
    );
    expect(earlySubmit.is_error, "the prehook denies before terminal execute").toBe(
      true,
    );
    expect(earlySubmit.content).toContain("cannot submit while 1 background");
    expect(earlySubmit.content).toContain("subagent:");
    expect(earlySubmit.content).toContain("(running)");

    const cancel = must(results.find((result) => result.tool_use_id === "tu_cancel"));
    expect(cancel.is_error, "the scripted recovery cancels the open session").toBe(
      false,
    );
    const askAdvisor = must(
      results.find((result) => result.tool_use_id === "tu_ask_advisor"),
    );
    expect(askAdvisor.is_error, "the advisor pass unlocks the final submit").toBe(
      false,
    );
    const finalSubmit = must(
      results.find((result) => result.tool_use_id === "tu_submit_final"),
    );
    expect(finalSubmit.is_error, "the final submit is allowed after cleanup").toBe(
      false,
    );
    const recoveryRequest = must(bossClient.requests.at(2));
    expect(
      recoveryRequest.messages.some((message) =>
        message.content.some(
          (block) =>
            block.type === "tool_result" &&
            block.tool_use_id === "tu_submit_early" &&
            block.content.includes("cannot submit while"),
        ),
      ),
      "the denied terminal result was delivered back to the model",
    ).toBe(true);
  });
});
