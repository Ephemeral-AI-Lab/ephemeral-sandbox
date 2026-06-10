import { describe, expect, it } from "vitest";

import { toolUses } from "@eos/contracts";

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
  unansweredToolUses,
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

// Budget guard: three live runs (~8 small provider calls, one interrupted);
// the mocked `wait` tool pins a deterministic mid-run window, so no
// assertion depends on stream timing or model prose.
describe.skipIf(!codex.available)("interrupt, wake, and steering over live codex (e2e)", () => {
  it(
    "interrupts a run mid-tool: cancelled outcome, recorded reason, every tool_use answered",
    { timeout: 120_000 },
    async () => {
      const wait = waitTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "pilot",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["wait"],
            maxTurns: 4,
            body: TERSE_BODY,
          },
        ],
        baseTools: [wait.definition],
      });
      const run = runtime.startRun({
        agentName: "pilot",
        initialMessages: [
          userMessage(
            'Call wait with {"ms": 60000}. After it returns, call submit_main_outcome with summary "waited".',
          ),
        ],
      });
      await wait.started;
      run.handle.interrupt("operator_stop");
      const outcome = await run.handle.outcome;
      if (outcome.status !== "cancelled") {
        throw new Error(`expected a cancelled outcome, got ${outcome.status}`);
      }
      expect(outcome.reason, "the interrupt reason rides the outcome").toBe("operator_stop");
      expect(wait.aborted(), "the in-flight tool call was aborted").toBeGreaterThanOrEqual(1);
      expect(
        unansweredToolUses(outcome.llm),
        "provider history stays valid: every tool_use is answered at cancellation",
      ).toEqual([]);

      await finishedRun(runtime, "pilot");
      expect(must(readTranscriptLines(run.transcriptPath).at(-1))).toMatchObject({
        kind: "run_finished",
        outcome_status: "cancelled",
        interrupt_reason: "operator_stop",
      });
    },
  );

  it(
    "applies a steer at the next loop boundary: the run redirects to the steered instruction",
    { timeout: 180_000 },
    async () => {
      const wait = waitTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "navigator",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["wait"],
            maxTurns: 6,
            body: TERSE_BODY,
          },
        ],
        baseTools: [wait.definition],
      });
      const run = runtime.startRun({
        agentName: "navigator",
        initialMessages: [
          userMessage(
            'Call wait with {"ms": 2500}. When it returns, follow the newest user instruction; if none arrived, call wait with {"ms": 2500} again.',
          ),
        ],
      });
      await wait.started;
      expect(
        run.handle.steer(
          userMessage(
            'New instruction: call submit_main_outcome with summary set to exactly "steered-ok".',
          ),
        ),
        "a steer is accepted while the run is live",
      ).toBe(true);

      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("steered-ok");
      const steeredIndex = userMessageIndex(outcome.llm, "New instruction");
      const firstAssistant = outcome.llm.findIndex(
        (message) => message.role === "assistant",
      );
      expect(steeredIndex, "the steer entered the conversation").toBeGreaterThanOrEqual(0);
      expect(
        steeredIndex,
        "the steer drained at a loop boundary, after the first assistant turn",
      ).toBeGreaterThan(firstAssistant);
    },
  );

  it(
    "wakes a run parked on a live session with a steer: cancel the sleeper, then submit",
    { timeout: 300_000 },
    async () => {
      const wait = waitTool();
      const { runtime, dataDir } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "watcher",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [
              "run_subagent",
              "list_background_sessions",
              "cancel_background_session",
            ],
            maxTurns: 8,
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
      const run = runtime.startRun({
        agentName: "watcher",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "sleeper" and prompt "hold".',
              '2. Reply with the plain text "standing by" and make no further tool calls until a new user instruction arrives.',
            ].join("\n"),
          ),
        ],
      });

      // The park is observable as a bare-text assistant turn after the spawn:
      // with a live session and no steers, the loop waits instead of calling
      // the provider again (auto-wait), so the steer below is the wake.
      await until(
        "the watcher to park on the live session",
        () => {
          try {
            const assistants = readTranscriptLines(run.transcriptPath).filter(
              (line) => line.kind === "assistant",
            );
            const last = assistants.at(-1);
            return (
              assistants.length >= 2 &&
              last !== undefined &&
              toolUses(last.message).length === 0
            );
          } catch {
            return false;
          }
        },
        120_000,
      );

      expect(
        run.handle.steer(
          userMessage(
            [
              "New instruction:",
              '1. Call cancel_background_session with type "subagent" and id set to the run_id returned by run_subagent.',
              '2. Call submit_main_outcome with summary "woke and cleaned".',
            ].join("\n"),
          ),
        ),
        "the steer is accepted by the parked run",
      ).toBe(true);

      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("woke and cleaned");
      expect(
        outcome.turns,
        "parking consumed no provider turns while waiting for the steer",
      ).toBeLessThanOrEqual(6);

      const sleeper = await finishedRun(runtime, "sleeper");
      expect(
        must(readTranscriptLines(runTranscriptPath(dataDir, sleeper.run_id)).at(-1)),
        "the woken run cancelled the sleeper as model_cancelled",
      ).toMatchObject({
        kind: "run_finished",
        outcome_status: "cancelled",
        interrupt_reason: "model_cancelled",
      });
    },
  );
});
