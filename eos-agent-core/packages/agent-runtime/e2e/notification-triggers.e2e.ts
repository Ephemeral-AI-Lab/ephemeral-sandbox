import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import { assistantText, toolUses, type Message } from "@eos/contracts";

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
  CODEWORD,
  HOLDER_BODY,
  TERSE_BODY,
  gateTool,
  lookupCodewordTool,
  rootHookConfigPath,
  runtimeFixture,
  submissionOf,
  until,
  userMessageIndex,
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

/** `node <repo>/.eos-agents/hooks/<name>` — absolute, so the fixture's temp cwd never matters. */
function triggerScriptCommand(name: string): string {
  const repoRoot = dirname(dirname(rootHookConfigPath()));
  return `node ${JSON.stringify(join(repoRoot, ".eos-agents", "hooks", name))}`;
}

/** The park probe from E2E-09/45: a bare-text last assistant turn. */
function parkedOnBareText(transcriptPath: string): boolean {
  try {
    const assistants = readTranscriptLines(transcriptPath).filter(
      (line) => line.kind === "assistant",
    );
    const last = assistants.at(-1);
    return (
      assistants.length >= 2 && last !== undefined && toolUses(last.message).length === 0
    );
  } catch {
    return false;
  }
}

/** Every drained `{type:"reminder"}` notification text, in arrival order. */
function reminderTexts(llm: readonly Message[]): string[] {
  return llm
    .filter((message) => message.role === "user")
    .map((message) => assistantText(message))
    .filter((text) => text.includes('"reminder"'));
}

// Budget guard: four live runs (~21 small provider calls). The reference
// trigger scripts are REAL spawned node processes wired through the opt-in
// `hookEntries` fixture option; the repo baseline hooks.json stays
// trigger-free, so the auto-wait trio keeps pinning the trigger-off shapes.
// Assertions are on drained reminder payloads, outcome status, and message
// order - never prose.
describe.skipIf(!codex.available)("notification triggers over live codex (e2e)", () => {
  it(
    "rescues the drifter spin: the TurnCompleted reminder names the terminal tool and the run completes instead of failing max_turns",
    { timeout: 240_000 },
    async () => {
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "drifter",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: [],
            maxTurns: 8,
            body: TERSE_BODY,
          },
        ],
        hookEntries: [
          {
            event: "TurnCompleted",
            hooks: [
              { type: "command", command: triggerScriptCommand("remind-terminal-submission.cjs") },
            ],
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "drifter",
        initialMessages: [
          userMessage(
            [
              '1. Reply with the plain text "standing by" and make no tool calls this turn.',
              "2. Then act on any system notifications you receive.",
            ].join("\n"),
          ),
        ],
      });

      const outcome = await run.handle.outcome;
      expect(
        outcome.status,
        "the reminder rescued the run the trigger-off baseline pins as failed: max_turns",
      ).toBe("completed");
      const firstAssistant = must(
        outcome.llm.find((message) => message.role === "assistant"),
      );
      expect(toolUses(firstAssistant), "the spin happened: turn 1 was bare text").toHaveLength(0);
      const reminder = assistantText(must(outcome.llm.at(2)));
      expect(
        reminder,
        "the reminder was drained before the next provider call, right after the bare-text turn",
      ).toContain('"reminder"');
      expect(reminder).toContain('"TurnCompleted"');
      expect(reminder, "the reminder names the profile's terminal tool").toContain(
        "submit_main_outcome",
      );
    },
  );

  it(
    "wakes a held park past timeout_ms: the IdleTimeout reminder lists the running session and the model recovers by cancelling it",
    { timeout: 300_000 },
    async () => {
      const gate = gateTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "idler",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent", "cancel_background_session"],
            maxTurns: 8,
            body: TERSE_BODY,
          },
          {
            name: "holder",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["hold"],
            maxTurns: 3,
            body: HOLDER_BODY,
          },
        ],
        baseTools: [gate.definition],
        hookEntries: [
          {
            event: "IdleParked",
            timeout_ms: 3_000,
            hooks: [{ type: "command", command: triggerScriptCommand("idle-wake.cjs") }],
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "idler",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "holder" and prompt "hold".',
              '2. Reply with the plain text "standing by" and make no further tool calls.',
              "3. Wait. When a system reminder about waiting on background work arrives, call",
              '   cancel_background_session with type "subagent" and the run_id from step 1.',
              '4. Then call submit_main_outcome with summary "woke and cancelled".',
            ].join("\n"),
          ),
        ],
      });

      // The gate is never released: only the idle-wake reminder can end the
      // park, so completing at all proves the timer fired and woke the run.
      await gate.started;
      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("woke and cancelled");

      const reminders = reminderTexts(outcome.llm);
      expect(reminders, "the park outlived timeout_ms exactly once").toHaveLength(1);
      expect(must(reminders.at(0))).toContain('"IdleTimeout"');
      expect(
        must(reminders.at(0)),
        "the reminder lists the running session by its native ref",
      ).toContain("subagent:");
      expect(
        userMessageIndex(outcome.llm, '"IdleTimeout"'),
        "the reminder woke the park; the cancellation settlement came after it",
      ).toBeLessThan(userMessageIndex(outcome.llm, '"session_settled"'));
    },
  );

  it(
    "stays silent when the wake comes first: a settlement inside timeout_ms means the idle script never speaks",
    { timeout: 300_000 },
    async () => {
      const gate = gateTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "idler",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["run_subagent"],
            maxTurns: 6,
            body: TERSE_BODY,
          },
          {
            name: "holder",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["hold"],
            maxTurns: 3,
            body: HOLDER_BODY,
          },
        ],
        baseTools: [gate.definition],
        hookEntries: [
          {
            event: "IdleParked",
            timeout_ms: 60_000,
            hooks: [{ type: "command", command: triggerScriptCommand("idle-wake.cjs") }],
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "idler",
        initialMessages: [
          userMessage(
            [
              '1. Call run_subagent with agent_name "holder" and prompt "hold".',
              '2. Reply with the plain text "standing by" and make no further tool calls.',
              "3. Wait for the session_settled notification for that run; do not poll with other tools.",
              '4. After it arrives, call submit_main_outcome with summary "idle then settled".',
            ].join("\n"),
          ),
        ],
      });

      await until(
        "the idler to park on the live session",
        () => parkedOnBareText(run.transcriptPath),
        120_000,
      );
      await gate.started;
      gate.release();
      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("idle then settled");
      expect(
        userMessageIndex(outcome.llm, '"session_settled"'),
        "the natural settlement woke the park",
      ).toBeGreaterThanOrEqual(0);
      expect(
        reminderTexts(outcome.llm),
        "the wake landed first, so the idle timer was cleared and no reminder exists",
      ).toEqual([]);
    },
  );

  it(
    "publishes exactly one budget reminder when the turn count hits 80% of max_turns",
    { timeout: 240_000 },
    async () => {
      const lookup = lookupCodewordTool();
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "counter",
            kind: "subagent",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["lookup_codeword"],
            maxTurns: 5,
            body: TERSE_BODY,
          },
        ],
        baseTools: [lookup.definition],
        hookEntries: [
          {
            event: "TurnCompleted",
            hooks: [{ type: "command", command: triggerScriptCommand("budget-reminder.cjs") }],
          },
        ],
      });
      const run = runtime.startRun({
        agentName: "counter",
        initialMessages: [
          userMessage(
            [
              "1. Call lookup_codeword.",
              "2. Call lookup_codeword again.",
              "3. Call lookup_codeword again.",
              "4. Call lookup_codeword again.",
              '5. Call submit_subagent_outcome with summary set to "counted: " followed by the codeword.',
            ].join("\n"),
          ),
        ],
      });

      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain(CODEWORD);
      expect(lookup.calls(), "all four budgeted lookups ran").toBeGreaterThanOrEqual(4);

      const reminders = reminderTexts(outcome.llm);
      expect(
        reminders,
        "equality with the threshold turn, not >=: exactly one budget reminder",
      ).toHaveLength(1);
      expect(must(reminders.at(0))).toContain("Turn 4 of 5 (80% of budget)");
      expect(
        must(reminders.at(0)),
        "the reminder names the profile's terminal tool",
      ).toContain("submit_subagent_outcome");
    },
  );
});
