import { describe, expect, it } from "vitest";

import { assistantText, toolUses } from "@eos/contracts";

import { runTranscriptPath } from "../src/transcript.js";
import { asString, must, readTranscriptLines, userMessage } from "../tests/support.js";
import {
  CODEX_CLIENT_ID,
  loadConfiguredCodexRuntime,
} from "./support/codex-runtime.js";
import {
  HELPER_BODY,
  PARALLEL_BODY,
  SLEEPER_BODY,
  TERSE_BODY,
  finishedRun,
  lookupCodewordTool,
  runtimeFixture,
  sessionSettledMessages,
  submissionOf,
  toolResultsIn,
  until,
  userMessageIndex,
  waitTool,
} from "./support/fixtures.js";

const codex = loadConfiguredCodexRuntime();

if (!codex.available) {
  console.warn(`agent-runtime e2e skipped: ${codex.reason}`);
}

function llmClientsPath(): string {
  if (!codex.available) {
    throw new Error("unreachable: the suite is skipped without credentials");
  }
  return codex.llmClientsPath;
}

/**
 * Exhausts its 2-turn budget on live tool-use loops; never submits. The
 * informational pin matters: the baseline 50% budget rung (ceil(2 * 0.5) = 1)
 * is drained before turn 2 saying "wrap up and submit", and this scenario
 * needs the second lookup to happen anyway.
 */
const SPINNER_BODY = [
  "You are the spinner.",
  "Call lookup_codeword, then call lookup_codeword again to double-check.",
  "Treat system notifications as informational; never submit before the",
  "second result.",
  "Only after the second result, call submit_subagent_outcome with",
  'summary "spun".',
].join(" ");

/** A middle agent that delegates once and then settles. */
const RELAY_BODY = [
  "You are the relay. Follow exactly:",
  '1. Call run_subagent with agent_name "leaf" and prompt "report in".',
  "2. Wait for its session_settled notification.",
  '3. Call submit_subagent_outcome with summary "relay done".',
].join(" ");

// Budget guard: four multi-run scenarios (~22 small provider calls across
// eight live runs, one cancelled). Assertions stay structural: parent
// links, settlement payload fields, transcript line kinds, notification
// counts, and cancellation reasons.
describe.skipIf(!codex.available)("subagent fan-out and nesting over live codex (e2e)", () => {
  it(
    "fans out two subagents in one batch and submits only after both settle",
    { timeout: 300_000 },
    async () => {
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "lead",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent"],
            maxTurns: 10,
            body: PARALLEL_BODY,
          },
          {
            name: "helper",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 3,
            body: HELPER_BODY,
          },
        ],
      });
      const lead = runtime.startRun({
        agentName: "lead",
        initialMessages: [
          userMessage(
            [
              '1. In one single assistant turn, call run_subagent twice - once with agent_name "helper" and prompt "first", once with agent_name "helper" and prompt "second".',
              "2. Wait until you have received a session_settled notification for BOTH runs.",
              '3. Call submit_main_outcome with summary "both done".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await lead.handle.outcome;
      expect(outcome.status).toBe("completed");

      const spawnTurn = outcome.llm.find(
        (message) =>
          message.role === "assistant" &&
          toolUses(message).filter((call) => call.name === "run_subagent").length === 2,
      );
      expect(spawnTurn, "both spawns left in one assistant turn").toBeDefined();

      const helpers = runtime.listRuns().filter((row) => row.agent_name === "helper");
      expect(helpers, "two sibling subagent runs registered").toHaveLength(2);
      for (const helper of helpers) {
        expect(helper.parent, `run ${helper.run_id} links to the lead`).toBe(lead.runId);
      }
      await until(
        "both helpers to finish",
        () =>
          runtime
            .listRuns()
            .filter((row) => row.agent_name === "helper" && row.status === "finished")
            .length === 2,
      );
      for (const helper of helpers) {
        expect(
          must(readTranscriptLines(runTranscriptPath(dataDir, helper.run_id)).at(-1)),
          `run ${helper.run_id} leaves a completed transcript`,
        ).toMatchObject({ kind: "run_finished", outcome_status: "completed" });
      }

      const settled = sessionSettledMessages(outcome.llm);
      expect(settled, "one settlement notification per child").toHaveLength(2);
      const results = toolResultsIn(outcome.llm);
      const launchResultIndexes = helpers.map((helper) =>
        results.findIndex((result) => result.content.includes(helper.run_id)),
      );
      for (const index of launchResultIndexes) {
        expect(index, "run_subagent returned each child id").toBeGreaterThanOrEqual(0);
      }
      const firstNotificationIndex = outcome.llm.findIndex((message) =>
        settled.includes(message),
      );
      const lastLaunchResultMessageIndex = Math.max(
        ...helpers.map((helper) =>
          outcome.llm.findIndex(
            (message) =>
              message.role === "user" &&
              message.content.some(
                (block) =>
                  block.type === "tool_result" &&
                  block.content.includes(helper.run_id),
              ),
          ),
        ),
      );
      expect(
        lastLaunchResultMessageIndex,
        "launch tool results arrive before child completion notifications",
      ).toBeLessThan(firstNotificationIndex);
      for (const helper of helpers) {
        expect(
          settled.some((message) => assistantText(message).includes(helper.run_id)),
          `the settlement for ${helper.run_id} reached the conversation`,
        ).toBe(true);
      }
    },
  );

  it(
    "cancels one background subagent while a sibling completes",
    { timeout: 300_000 },
    async () => {
      const wait = waitTool();
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "coordinator",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [
              "run_subagent",
              "list_background_sessions",
              "cancel_background_session",
            ],
            maxTurns: 12,
            body: PARALLEL_BODY,
          },
          {
            name: "helper",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 3,
            body: HELPER_BODY,
          },
          {
            name: "sleeper",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["wait"],
            maxTurns: 3,
            body: SLEEPER_BODY,
          },
        ],
        baseTools: [wait.definition],
      });
      const coordinator = runtime.startRun({
        agentName: "coordinator",
        initialMessages: [
          userMessage(
            [
              '1. Your first assistant response must contain exactly two run_subagent tool calls in the same response - one with agent_name "helper" and prompt "finish", one with agent_name "sleeper" and prompt "hold". Do not wait for the first result before issuing the second call.',
              "2. Call list_background_sessions.",
              '3. Call cancel_background_session with type "subagent" and id set to the run_id returned by the sleeper call.',
              "4. Wait until you have received completion notifications for BOTH child runs.",
              '5. Call submit_main_outcome with summary "mixed statuses".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await coordinator.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("mixed statuses");

      const spawnTurn = outcome.llm.find(
        (message) =>
          message.role === "assistant" &&
          toolUses(message).filter((call) => call.name === "run_subagent").length === 2,
      );
      expect(spawnTurn, "both child runs launched in one assistant turn").toBeDefined();

      const helper = await finishedRun(runtime, "helper");
      const sleeper = await finishedRun(runtime, "sleeper");
      expect(helper.parent, "the completed child links to the coordinator").toBe(
        coordinator.runId,
      );
      expect(sleeper.parent, "the cancelled child links to the coordinator").toBe(
        coordinator.runId,
      );

      expect(
        must(readTranscriptLines(runTranscriptPath(dataDir, helper.run_id)).at(-1)),
        "the sibling helper completed normally",
      ).toMatchObject({ kind: "run_finished", outcome_status: "completed" });
      expect(
        must(readTranscriptLines(runTranscriptPath(dataDir, sleeper.run_id)).at(-1)),
        "the background cancel records the model-initiated reason",
      ).toMatchObject({
        kind: "run_finished",
        outcome_status: "cancelled",
        interrupt_reason: "model_cancelled",
      });

      expect(
        toolResultsIn(outcome.llm).some(
          (result) =>
            !result.is_error &&
            result.content.includes("background session subagent:") &&
            result.content.includes("cancelled"),
        ),
        "cancel_background_session acknowledged the cancelled subagent",
      ).toBe(true);
      const settled = sessionSettledMessages(outcome.llm);
      for (const [runId, status] of [
        [helper.run_id, "completed"],
        [sleeper.run_id, "cancelled"],
      ] as const) {
        const message = settled.find((candidate) =>
          assistantText(candidate).includes(runId),
        );
        expect(message, `the ${status} settlement reached the coordinator`).toBeDefined();
        expect(assistantText(must(message)), `settlement for ${runId} has status`).toContain(
          `"status":"${status}"`,
        );
      }
    },
  );

  it(
    "surfaces a subagent that exhausts its turn budget as a failed settlement",
    { timeout: 240_000 },
    async () => {
      const lookup = lookupCodewordTool();
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "overseer",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent"],
            maxTurns: 8,
            body: TERSE_BODY,
          },
          {
            name: "spinner",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["lookup_codeword"],
            maxTurns: 2,
            body: SPINNER_BODY,
          },
        ],
        baseTools: [lookup.definition],
      });
      const overseer = runtime.startRun({
        agentName: "overseer",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "spinner" and prompt "go".',
              "2. Wait for its session_settled notification. It will report a failure; that is expected.",
              '3. Call submit_main_outcome with summary "observed the failure".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await overseer.handle.outcome;
      expect(outcome.status, "the caller completes despite the child failure").toBe(
        "completed",
      );

      const spinner = await finishedRun(runtime, "spinner");
      const lines = readTranscriptLines(runTranscriptPath(dataDir, spinner.run_id));
      expect(
        lines.filter((line) => line.kind === "assistant").length,
        "the spinner spent its whole budget on live tool-use turns",
      ).toBeGreaterThanOrEqual(2);
      expect(must(lines.at(-1)), "the spinner's own transcript records the failure").toMatchObject({
        kind: "run_finished",
        outcome_status: "failed",
      });
      expect(lookup.calls(), "the spinner's tool loops really executed").toBeGreaterThanOrEqual(2);

      const settledIndex = userMessageIndex(outcome.llm, '"session_settled"');
      expect(settledIndex, "the failed settlement was drained").toBeGreaterThanOrEqual(0);
      const settledText = assistantText(must(outcome.llm.at(settledIndex)));
      expect(settledText, "the settlement names the child").toContain(spinner.run_id);
      expect(settledText, "the settlement carries the failed status").toContain(
        '"status":"failed"',
      );
      expect(settledText, "the summary is the max_turns failure message").toContain(
        "turn budget",
      );
    },
  );

  it(
    "nests subagents: the child spawns its own helper with isolated notifications",
    { timeout: 300_000 },
    async () => {
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "chief",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent"],
            maxTurns: 8,
            body: TERSE_BODY,
          },
          {
            name: "relay",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent"],
            maxTurns: 8,
            body: RELAY_BODY,
          },
          {
            name: "leaf",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 3,
            body: HELPER_BODY,
          },
        ],
      });
      const chief = runtime.startRun({
        agentName: "chief",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "relay" and prompt "delegate".',
              "2. Wait for its session_settled notification.",
              '3. Call submit_main_outcome with summary "chain done".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await chief.handle.outcome;
      expect(outcome.status).toBe("completed");

      const relay = await finishedRun(runtime, "relay");
      const leaf = await finishedRun(runtime, "leaf");
      expect(relay.parent, "the middle run links to the chief").toBe(chief.runId);
      expect(leaf.parent, "the grandchild links to the middle run, not the chief").toBe(
        relay.run_id,
      );
      for (const row of [relay, leaf]) {
        expect(
          must(readTranscriptLines(runTranscriptPath(dataDir, row.run_id)).at(-1)),
          `run ${row.run_id} completed`,
        ).toMatchObject({ kind: "run_finished", outcome_status: "completed" });
      }

      // One inbox/supervisor pair per run (04.5 decision 1): the chief sees
      // exactly its own child's settlement; the grandchild's settlement
      // stays in the relay's conversation.
      const settled = sessionSettledMessages(outcome.llm);
      expect(settled, "the chief drained exactly one settlement").toHaveLength(1);
      const settledText = assistantText(must(settled.at(0)));
      expect(settledText).toContain(relay.run_id);
      expect(
        settledText.includes(leaf.run_id),
        "the grandchild's settlement never reached the chief's inbox",
      ).toBe(false);
    },
  );
});
