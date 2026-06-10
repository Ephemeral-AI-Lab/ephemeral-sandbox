import { describe, expect, it } from "vitest";

import type { Message } from "@eos/contracts";
import { startAgentRun, type AgentEvent, type AgentRunHandle } from "@eos/engine";
import { buildToolExecutor, type ToolDefinition } from "@eos/tool";
import { scriptedRunState } from "@eos/testkit";

import { asString, must, userMessage } from "../tests/support.js";
import { loadConfiguredCodexRuntime } from "./support/codex-runtime.js";
import {
  CODEWORD,
  finishTaskTool,
  lookupCodewordTool,
  submissionOf,
  unansweredToolUses,
  waitTool,
} from "./support/fixtures.js";

const codex = loadConfiguredCodexRuntime();

if (!codex.available) {
  console.warn(`agent-runtime e2e skipped: ${codex.reason}`);
}

const SYSTEM_PROMPT = [
  "You are a terse test agent.",
  "Follow the user's instructions exactly and in order.",
  "Make at most one tool call per assistant turn and write no prose.",
].join(" ");

/**
 * Engine-direct run over the live client: inside the runtime the transcript
 * subscriber is the stream's single consumer (Phase 04.5 §2.5), so raw
 * `AgentEvent` ordering over live SSE is only observable at this seam.
 */
function liveRun(definitions: ToolDefinition[], messages: Message[]): AgentRunHandle {
  if (!codex.available) {
    throw new Error("unreachable: the suite is skipped without credentials");
  }
  const { binding } = codex;
  return startAgentRun({
    llmClient: binding.client,
    tools: buildToolExecutor({ runState: scriptedRunState("main"), definitions }),
    model: binding.model_id,
    reasoningEffort: binding.reasoning_effort,
    systemPrompt: SYSTEM_PROMPT,
    maxTurns: 4,
    initialMessages: messages,
  });
}

// Budget guard: ~4 small provider calls (one of them aborted mid-wait);
// assertions are on event types, ids, counts, and ordering - never prose.
describe.skipIf(!codex.available)("engine event stream over live codex SSE (e2e)", () => {
  it(
    "streams one ordered single-consumer event sequence ending in run_finished",
    { timeout: 180_000 },
    async () => {
      const lookup = lookupCodewordTool();
      const handle = liveRun(
        [lookup.definition, finishTaskTool()],
        [
          userMessage(
            [
              "1. Call lookup_codeword.",
              "2. Call finish_task with summary set to exactly the codeword from the result.",
            ].join("\n"),
          ),
        ],
      );

      const events: AgentEvent[] = [];
      for await (const event of handle.events) events.push(event);

      expect(must(events.at(0)), "the first event opens turn 1").toMatchObject({
        type: "turn_started",
        turn: 1,
      });
      const turnNumbers = events.flatMap((event) =>
        event.type === "turn_started" ? [event.turn] : [],
      );
      expect(
        turnNumbers,
        "turn numbers count up from 1",
      ).toEqual(turnNumbers.map((_, index) => index + 1));
      expect(
        events.filter((event) => event.type === "assistant_message_complete").length,
        "exactly one completion per provider turn",
      ).toBe(turnNumbers.length);
      expect(
        events.filter(
          (event) =>
            event.type === "assistant_text_delta" ||
            event.type === "reasoning_delta" ||
            event.type === "tool_use_delta",
        ).length,
        "the live SSE stream produced incremental events",
      ).toBeGreaterThan(0);
      expect(
        events.some(
          (event) =>
            event.type === "tool_use_delta" && event.name === "lookup_codeword",
        ),
        "the assembled tool call surfaced as a delta",
      ).toBe(true);

      const started = events.filter((event) => event.type === "tool_execution_started");
      const completed = events.filter(
        (event) => event.type === "tool_execution_completed",
      );
      expect(
        completed.map((event) => event.tool_use_id).sort(),
        "every dispatched tool call settled",
      ).toEqual(started.map((event) => event.tool_use_id).sort());
      for (const event of completed) {
        expect(
          event.tool_end_time,
          `tool ${event.name} timing brackets execute()`,
        ).toBeGreaterThanOrEqual(event.tool_start_time);
      }
      const lookupDone = must(completed.find((event) => event.name === "lookup_codeword"));
      expect(lookupDone.is_error, "the lookup settled cleanly").toBe(false);
      expect(lookupDone.is_terminal, "the lookup is not terminal").toBe(false);
      expect(lookupDone.output, "the lookup output rode the event").toContain(CODEWORD);
      const terminalDone = must(completed.find((event) => event.name === "finish_task"));
      expect(terminalDone.is_terminal, "the submission is terminal").toBe(true);
      expect(
        events.indexOf(lookupDone),
        "the lookup settled before the terminal call",
      ).toBeLessThan(events.indexOf(terminalDone));

      const last = must(events.at(-1));
      if (last.type !== "run_finished") {
        throw new Error(`expected run_finished last, got ${last.type}`);
      }
      expect(
        events.filter((event) => event.type === "run_finished"),
        "run_finished appears exactly once",
      ).toHaveLength(1);
      const outcome = await handle.outcome;
      expect(outcome, "the outcome IS the run_finished payload").toBe(last.outcome);
      expect(outcome.status).toBe("completed");
      expect(
        () => handle.events[Symbol.asyncIterator](),
        "the stream supports a single consumer",
      ).toThrow(/single consumer/);
    },
  );

  it(
    "salvages an interrupt into provider-valid history that a live restart accepts",
    { timeout: 180_000 },
    async () => {
      const wait = waitTool();
      const finish = finishTaskTool();
      const first = liveRun(
        [wait.definition, finish],
        [
          userMessage(
            'Call wait with {"ms": 60000}. After it returns, call finish_task with summary "waited".',
          ),
        ],
      );
      await wait.started;
      first.interrupt("redirect");
      const outcome = await first.outcome;
      if (outcome.status !== "cancelled") {
        throw new Error(`expected a cancelled outcome, got ${outcome.status}`);
      }
      expect(outcome.reason).toBe("redirect");
      expect(
        unansweredToolUses(outcome.llm),
        "the aborted call is answered in the salvaged history",
      ).toEqual([]);

      const second = liveRun(
        [wait.definition, finish],
        [
          ...outcome.llm,
          userMessage('Stop waiting. Call finish_task now with summary set to exactly "resumed".'),
        ],
      );
      const resumed = await second.outcome;
      expect(
        resumed.status,
        "the live provider accepted the salvaged history as restart input",
      ).toBe("completed");
      expect(asString(submissionOf(resumed).summary)).toContain("resumed");
    },
  );
});
