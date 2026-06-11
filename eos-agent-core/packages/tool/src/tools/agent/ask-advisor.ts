import {
  JsonObjectSchema,
  type JsonObject,
  type JsonValue,
} from "@eos/contracts";
import type { AgentRunOutcome } from "@eos/engine";
import { z } from "zod";

import type { ToolDefinition, ToolOutcome } from "../../contract.js";
import { defineTool } from "../../define.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import type { AgentRunCalls, AgentToolUserMessage } from "./index.js";

/** The single site that owns the advisor profile's magic name (§2.6). */
export const ADVISOR_AGENT_NAME = "advisor";

const MAX_READ_BYTES = 262_144;

function userText(text: string): AgentToolUserMessage {
  return { role: "user", content: [{ type: "text", text }] };
}

const AskAdvisorInputSchema = z.object({
  /** The terminal tool the caller intends to call next. */
  tool_name: z.string().min(1),
  /** The exact payload the caller intends to submit. */
  payload: JsonObjectSchema.optional(),
});

export function askAdvisorTool(calls: AgentRunCalls): ToolDefinition {
  return defineTool({
    name: "ask_advisor",
    description: descriptionPrompt("ask_advisor"),
    input: AskAdvisorInputSchema,
    execute: async (input, ctx) => {
      const advisorPrompt = calls.advisorPromptFor(input.tool_name);
      if (advisorPrompt === undefined) {
        return {
          content: `tool ${input.tool_name} does not have an advisor prompt`,
          isError: true,
        };
      }
      const callerTranscript = await readWholeTranscript(
        calls,
        ctx.meta.run.transcript_path,
      );
      const target: JsonObject = {
        tool_name: input.tool_name,
        payload: input.payload ?? {},
      };
      const advisor = calls.startRun({
        agentName: ADVISOR_AGENT_NAME,
        initialMessages: [
          userText(callerTranscript),
          userText(
            `${advisorPrompt} Please verify against the below tool name + payload\n${canonicalJson(target)}`,
          ),
        ],
        // The advisor dies with this tool call's own execution scope (§13.6).
        signal: ctx.signal,
      });
      return mapAdvisorOutcome(await advisor.handle.outcome);
    },
  });
}

function canonicalJson(value: JsonValue): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`);
    return `{${entries.join(",")}}`;
  }
  return JSON.stringify(value);
}

async function readWholeTranscript(
  calls: AgentRunCalls,
  path: string,
): Promise<string> {
  let offset = 0;
  let data = "";
  for (;;) {
    const read = await calls.readTranscriptFile(path, offset, MAX_READ_BYTES);
    data += read.data;
    offset = read.next_offset;
    if (read.eof) return data;
  }
}

function mapAdvisorOutcome(outcome: AgentRunOutcome): ToolOutcome {
  switch (outcome.status) {
    case "completed":
      return { content: outcome.submission ?? "advisor run completed without a submission" };
    case "cancelled":
      return { content: `advisor run cancelled: ${outcome.reason}`, isError: true };
    case "failed":
      return { content: `advisor run failed: ${outcome.failure.message}`, isError: true };
  }
}
