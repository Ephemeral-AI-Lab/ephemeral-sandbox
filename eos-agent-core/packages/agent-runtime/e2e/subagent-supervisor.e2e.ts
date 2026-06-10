import { describe, expect, it } from "vitest";

import { runTranscriptPath } from "../src/transcript.js";
import {
  asString,
  must,
  readTranscriptLines,
  userMessage,
} from "../tests/support.js";
import {
  CODEX_CLIENT_ID,
  loadConfiguredCodexRuntime,
} from "./support/codex-runtime.js";
import {
  SLEEPER_BODY,
  TERSE_BODY,
  finishedRun,
  runtimeFixture,
  submissionOf,
  toolResultsIn,
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

const HELPER_BODY = [
  "You are the helper.",
  "Immediately call submit_subagent_outcome exactly once with summary set to",
  'exactly "helper finished". Do not call any other tool.',
].join(" ");

// Budget guard: two multi-turn live runs (~10 small provider calls total);
// every assertion is structural - registry rows, transcript line kinds,
// interrupt reasons, tool_result flags - never model prose.
describe.skipIf(!codex.available)("background supervisor and agent tools over live codex (e2e)", () => {
  it(
    "runs the subagent round-trip: spawn by name, settle notification, transcript read, submit",
    { timeout: 300_000 },
    async () => {
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "lead",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [
              "run_subagent",
              "read_agent_run_transcript",
              "list_background_sessions",
              "cancel_background_session",
            ],
            maxTurns: 10,
            body: TERSE_BODY,
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
              '1. Call run_subagent with agent_name "helper" and prompt "report in".',
              "2. Wait for the session_settled notification for that run; do not poll with other tools.",
              "3. Call read_agent_run_transcript with the run_id returned by step 1.",
              '4. Call submit_main_outcome with summary "delegation complete".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await lead.handle.outcome;
      expect(outcome.status).toBe("completed");

      const helper = await finishedRun(runtime, "helper");
      expect(helper.parent, "the caller is the parent link").toBe(lead.runId);
      expect(helper.agent_kind).toBe("subagent");
      expect(
        must(readTranscriptLines(runTranscriptPath(dataDir, helper.run_id)).at(-1)),
        "the subagent run leaves its own flushed transcript",
      ).toMatchObject({ kind: "run_finished", outcome_status: "completed" });

      // The quoted JSON form: the instruction prompt itself names the
      // notification type in prose, so the bare word would match message 0.
      const settledIndex = userMessageIndex(outcome.llm, '"session_settled"');
      expect(
        settledIndex,
        "the settlement notification was drained into the conversation",
      ).toBeGreaterThanOrEqual(0);
      const settled = must(outcome.llm.at(settledIndex));
      expect(
        settled.content.some(
          (block) => block.type === "text" && block.text.includes(helper.run_id),
        ),
        "the notification names the subagent run",
      ).toBe(true);

      const results = toolResultsIn(outcome.llm);
      expect(
        results.some((result) => result.content.includes(helper.run_id)),
        "run_subagent returned the run id immediately",
      ).toBe(true);
      expect(
        results.some(
          (result) => !result.is_error && result.content.includes("helper finished"),
        ),
        "the transcript read returned the helper's flushed submission",
      ).toBe(true);
    },
  );

  it(
    "guards submission while a session is open, then records a model-initiated cancel",
    { timeout: 300_000 },
    async () => {
      const wait = waitTool();
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "boss",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [
              "run_subagent",
              "list_background_sessions",
              "cancel_background_session",
            ],
            maxTurns: 10,
            body: TERSE_BODY,
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
      const boss = runtime.startRun({
        agentName: "boss",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "sleeper" and prompt "begin".',
              '2. Call submit_main_outcome with summary "too early". It will fail with an error because a session is open; that is expected.',
              "3. Call list_background_sessions.",
              '4. Call cancel_background_session with type "subagent" and id set to the run_id from step 1.',
              '5. Call submit_main_outcome with summary "cleaned up".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await boss.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("cleaned up");

      const results = toolResultsIn(outcome.llm);
      expect(
        results.some(
          (result) => result.is_error && result.content.includes("cannot submit while"),
        ),
        "the open session blocked the early submission",
      ).toBe(true);
      expect(
        results.some(
          (result) => !result.is_error && result.content.includes("cancelled"),
        ),
        "the cancel tool acknowledged",
      ).toBe(true);

      const sleeper = await finishedRun(runtime, "sleeper");
      expect(
        must(readTranscriptLines(runTranscriptPath(dataDir, sleeper.run_id)).at(-1)),
        "a model-initiated cancel is recorded distinctly from the disposal cascade",
      ).toMatchObject({
        kind: "run_finished",
        outcome_status: "cancelled",
        interrupt_reason: "model_cancelled",
      });
    },
  );
});
