import { describe, expect, it } from "vitest";

import { assistantText, type JsonObject } from "@eos/contracts";
import { eosAgentsPath, scriptedTool } from "@eos/testkit";
import type { ToolDefinition } from "@eos/tool";

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
  TERSE_BODY,
  runtimeFixture,
  submissionOf,
  toolResultsIn,
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

function readNoteTool(): ToolDefinition {
  return scriptedTool({
    name: "read_note",
    description: "Read the shared note. Takes no arguments.",
    execute: () => Promise.resolve({ content: "the note says 42" }),
  });
}

// Budget guard: two live runs (~7 small provider calls). Hooks are REAL
// spawned node processes gating REAL model tool calls; assertions are on
// result flags, recorded tool inputs, notification payloads, and
// `metadata.hook_warnings` - never prose.
describe.skipIf(!codex.available)("hook verification over live codex (e2e)", () => {
  it(
    "denies a live tool call by transcript evidence, watches the model recover, and republishes hook context",
    { timeout: 240_000 },
    async () => {
      // The checked-in write-note gate: deny `write_note` until a
      // `read_note` call is on the transcript (quoted-JSON needle, so the
      // prompt's prose mention of the tool can never satisfy it).
      let writeExecutions = 0;
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "scribe",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["read_note", "write_note"],
            maxTurns: 8,
            body: TERSE_BODY,
          },
        ],
        baseTools: [
          readNoteTool(),
          scriptedTool({
            name: "write_note",
            description: 'Write text to the shared note. Input: { "text": string }.',
            execute: () => {
              writeExecutions += 1;
              return Promise.resolve({ content: "wrote" });
            },
          }),
        ],
        extraHooksPath: eosAgentsPath("tests/hooks/write-note-gate.json"),
      });
      const run = runtime.startRun({
        agentName: "scribe",
        initialMessages: [
          userMessage(
            [
              '1. Call write_note with {"text": "first"}. A policy hook will deny it; that is expected.',
              "2. Call read_note.",
              '3. Call write_note with {"text": "second"}.',
              '4. Call submit_main_outcome with summary "noted".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");

      const results = toolResultsIn(outcome.llm);
      const deniedIndex = results.findIndex(
        (result) =>
          result.is_error && result.content.includes("requires reading the note first"),
      );
      const readIndex = results.findIndex(
        (result) => !result.is_error && result.content.includes("the note says 42"),
      );
      const allowedIndex = results.findIndex(
        (result) => !result.is_error && result.content === "wrote",
      );
      expect(deniedIndex, "the hook denied the premature write with its stderr reason").toBeGreaterThanOrEqual(0);
      expect(readIndex, "the model recovered by reading the note").toBeGreaterThanOrEqual(0);
      expect(allowedIndex, "the post-read write passed the hook").toBeGreaterThanOrEqual(0);
      expect(deniedIndex, "deny came before the read").toBeLessThan(readIndex);
      expect(readIndex, "the read came before the allowed write").toBeLessThan(allowedIndex);
      expect(
        writeExecutions,
        "the denied call never reached execute(); only the allowed write ran",
      ).toBe(1);

      const contextIndex = userMessageIndex(outcome.llm, '"hook_context"');
      expect(
        contextIndex,
        "the hook's additionalContext republished as a notification at the next boundary",
      ).toBeGreaterThanOrEqual(0);
      expect(assistantText(must(outcome.llm.at(contextIndex)))).toContain(
        "note was read before writing",
      );
    },
  );

  it(
    "rewrites a live tool call's input through updatedInput and records warning passthroughs",
    { timeout: 240_000 },
    async () => {
      // Checked-in pair: rewrite write_note's input through updatedInput
      // (re-validated by the tool's own schema) and answer read_note's
      // hook with garbage stdout on exit 0 - passthrough plus a warning.
      const written: JsonObject[] = [];
      const { runtime } = runtimeFixture({
        llmClientsPath: llmClientsPath(),
        profiles: [
          {
            name: "redactor",
            kind: "main",
            llmClientId: CODEX_CLIENT_ID,
            allowed: ["read_note", "write_note"],
            maxTurns: 8,
            body: TERSE_BODY,
          },
        ],
        baseTools: [
          readNoteTool(),
          scriptedTool({
            name: "write_note",
            description: 'Write text to the shared note. Input: { "text": string }.',
            execute: (input) => {
              written.push(input);
              return Promise.resolve({ content: "wrote" });
            },
          }),
        ],
        extraHooksPath: eosAgentsPath("tests/hooks/rewrite-and-garbage.json"),
      });
      const run = runtime.startRun({
        agentName: "redactor",
        initialMessages: [
          userMessage(
            [
              "1. Call read_note.",
              '2. Call write_note with {"text": "original secret"}.',
              '3. Call submit_main_outcome with summary "rewritten".',
            ].join("\n"),
          ),
        ],
      });
      const outcome = await run.handle.outcome;
      expect(outcome.status).toBe("completed");
      expect(asString(submissionOf(outcome).summary)).toContain("rewritten");

      expect(written, "the write executed exactly once").toHaveLength(1);
      expect(
        asString(must(written.at(0)).text),
        "execute() saw the hook's rewritten input, not the model's",
      ).toBe("REDACTED-BY-HOOK");

      const lines = readTranscriptLines(run.transcriptPath);
      const readResult = must(
        lines
          .flatMap((line) => (line.kind === "tool_result" ? [line.result] : []))
          .find(
            (result) =>
              typeof result.content === "string" &&
              result.content.includes("the note says 42"),
          ),
      );
      const warnings = readResult.metadata?.hook_warnings;
      expect(Array.isArray(warnings), "the garbage hook left an operator warning").toBe(
        true,
      );
      expect(
        JSON.stringify(warnings),
        "the warning names the malformed stdout",
      ).toContain("hook stdout was not JSON");
    },
  );
});
