import { describe, expect, it } from "vitest";

import {
  asString,
  must,
  readTranscriptLines,
  tempDir,
  userMessage,
} from "../tests/support.js";
import {
  CODEX_CLIENT_ID,
  loadConfiguredCodexRuntime,
  writeCorruptedCodexConfig,
} from "./support/codex-runtime.js";
import {
  CODEWORD,
  TERSE_BODY,
  finishedRun,
  lookupCodewordTool,
  runtimeFixture,
  submissionOf,
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

// Budget guard: ~6 small provider calls across the file (one of them a fast
// auth rejection); every assertion is structural - statuses, failure kinds,
// transcript line kinds, ids - never model prose.
describe.skipIf(!codex.available)("agent loop over live codex (e2e)", () => {
  it(
    "completes a main run through the terminal tool and leaves an ordered transcript",
    { timeout: 120_000 },
    async () => {
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "sage",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 4,
            body: TERSE_BODY,
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "sage",
        initialMessages: [
          userMessage(
            'Call submit_main_outcome exactly once with summary set to exactly "pong".',
          ),
        ],
      });
      expect(
        () => run.handle.events[Symbol.asyncIterator](),
        "the runtime is the event stream's single consumer (§2.5)",
      ).toThrow(/single consumer/);

      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary).toLowerCase()).toContain("pong");
      expect(outcome.turns, "at least one live provider turn ran").toBeGreaterThanOrEqual(1);
      expect(outcome.usage.input_tokens, "live input usage accounted").toBeGreaterThan(0);
      expect(outcome.usage.output_tokens, "live output usage accounted").toBeGreaterThan(0);
      expect(
        run.handle.steer(userMessage("too late")),
        "a steer after finish is refused",
      ).toBe(false);

      const row = await finishedRun(runtime, "sage");
      expect(row.agent_kind).toBe("main");
      const lines = readTranscriptLines(run.transcriptPath);
      expect(
        lines.map((line) => line.seq),
        "seq stays dense across the live run",
      ).toEqual(lines.map((_, index) => index));
      expect(must(lines.at(0))).toMatchObject({ kind: "user", origin: "initial" });
      expect(
        lines.filter((line) => line.kind === "run_finished"),
        "exactly one terminal transcript line",
      ).toHaveLength(1);
      expect(must(lines.at(-1))).toMatchObject({
        kind: "run_finished",
        outcome_status: "completed",
      });
      expect(lines.some((line) => line.kind === "assistant"), "assistant lines recorded").toBe(true);
      expect(lines.some((line) => line.kind === "tool_result"), "tool lines recorded").toBe(true);
    },
  );

  it(
    "round-trips a mocked tool: the looked-up codeword reaches the submission",
    { timeout: 180_000 },
    async () => {
      const lookup = lookupCodewordTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "scout",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["lookup_codeword"],
            maxTurns: 6,
            body: TERSE_BODY,
          },
        ],
        baseTools: [lookup.definition],
      });
      const run = runtime.startRun({
        agentName: "scout",
        initialMessages: [
          userMessage(
            [
              "1. Call lookup_codeword.",
              "2. Call submit_main_outcome with summary set to exactly the codeword from the result.",
            ].join("\n"),
          ),
        ],
      });
      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(lookup.calls(), "the mocked tool executed").toBeGreaterThanOrEqual(1);
      expect(asString(submissionOf(outcome).summary)).toContain(CODEWORD);

      await finishedRun(runtime, "scout");
      const results = readTranscriptLines(run.transcriptPath).flatMap((line) =>
        line.kind === "tool_result" ? [line.result] : [],
      );
      const lookupResult = must(
        results.find(
          (result) =>
            !result.is_terminal &&
            typeof result.content === "string" &&
            result.content.includes(CODEWORD),
        ),
      );
      expect(lookupResult.is_error, "the lookup result is clean").toBe(false);
      const terminal = must(results.find((result) => result.is_terminal));
      expect(
        results.indexOf(lookupResult),
        "the lookup answered before the terminal submission",
      ).toBeLessThan(results.indexOf(terminal));
    },
  );

  it(
    "fails with kind max_turns when the budget is spent before a terminal call",
    { timeout: 120_000 },
    async () => {
      const lookup = lookupCodewordTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "flash",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["lookup_codeword"],
            maxTurns: 1,
            body: TERSE_BODY,
          },
        ],
        baseTools: [lookup.definition],
      });
      const run = runtime.startRun({
        agentName: "flash",
        initialMessages: [
          userMessage(
            [
              "1. Call lookup_codeword.",
              '2. Only after seeing its result, call submit_main_outcome with summary "done".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await run.handle.outcome;
      if (outcome.status !== "failed") {
        throw new Error(`expected a failed outcome, got ${outcome.status}`);
      }
      expect(outcome.failure.kind).toBe("max_turns");
      expect(outcome.turns, "the budget was one provider turn").toBe(1);

      await finishedRun(runtime, "flash");
      expect(must(readTranscriptLines(run.transcriptPath).at(-1))).toMatchObject({
        kind: "run_finished",
        outcome_status: "failed",
      });
    },
  );

  it(
    "classifies a live auth rejection as a provider_error failure",
    { timeout: 120_000 },
    async () => {
      const configPath = writeCorruptedCodexConfig(
        tempDir("eos-corrupt-codex-"),
        llmClientsPath(),
      );
      const { runtime } = runtimeFixture({
        llmClientsPath: configPath,
        profiles: [
          {
            name: "ghost",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 2,
            body: TERSE_BODY,
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "ghost",
        initialMessages: [userMessage("Reply with one word.")],
      });
      const outcome = await run.handle.outcome;
      if (outcome.status !== "failed") {
        throw new Error(`expected a failed outcome, got ${outcome.status}`);
      }
      expect(outcome.failure.kind).toBe("provider_error");

      await finishedRun(runtime, "ghost");
      expect(must(readTranscriptLines(run.transcriptPath).at(-1))).toMatchObject({
        kind: "run_finished",
        outcome_status: "failed",
      });
    },
  );
});
