import {
  createAgentOutcomeFn,
  defineTool,
  type AgentOutcome,
  type HookEntry,
  type JsonObject,
  type JsonValue,
  type Message,
  type ToolDefinition,
  type UserMessage,
} from "eos-agent-sdk";
import { z } from "zod";

import type { AgentFactory } from "../../agents/agent-factory.js";
import { canonicalJson, type AdvisorPassRegistry } from "../../agents/advisor-pass-registry.js";

export const ADVISOR_AGENT_NAME = "advisor";
export const SUBMIT_ADVISOR_OUTCOME = "submit_advisor_outcome";

const SUBMIT_ADVISOR_DESCRIPTION =
  "Finish the advisor run by submitting a pass/fail verdict on the caller's intended terminal submission.";

const AdvisorVerdictSchema = z.object({
  verdict: z.enum(["pass", "fail"]),
  reason: z.string().min(1),
});
type AdvisorVerdict = z.infer<typeof AdvisorVerdictSchema>;

export const advisorOutcomeFn = createAgentOutcomeFn({
  name: SUBMIT_ADVISOR_OUTCOME,
  description: SUBMIT_ADVISOR_DESCRIPTION,
  schema: AdvisorVerdictSchema,
});

const JsonValueSchema: z.ZodType<JsonValue> = z.lazy(() =>
  z.union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.array(JsonValueSchema),
    JsonObjectSchema,
  ]),
);
const JsonObjectSchema: z.ZodType<JsonObject> = z.record(z.string(), JsonValueSchema);

/**
 * Factory-injected from the advisory binding (never profile-selected). Launches
 * the configured advisor over the caller's transcript plus the exact intended
 * submission; a `pass` verdict records the run+submission in the shared pass
 * registry, which the terminal gate then consults.
 */
export function askAdvisor(
  agents: AgentFactory,
  advisoryPrompt: string,
  passes: AdvisorPassRegistry,
): ToolDefinition {
  return defineTool({
    name: "ask_advisor",
    description: "Ask the configured advisor to review a terminal submission.",
    input: z.object({
      tool_name: z.string().min(1),
      payload: JsonObjectSchema,
    }),
    execute: async (input, ctx) => {
      const advisor = agents.create<AdvisorVerdict>(ADVISOR_AGENT_NAME, advisorOutcomeFn);
      const run = advisor.start({
        messages: [
          userText(renderCallerMessages(ctx.llmMessages)),
          userText(`${advisoryPrompt} Verify against the exact target below.\n${canonicalJson(input)}`),
        ],
      });
      ctx.signal.addEventListener("abort", () => {
        run.interrupt();
      });
      const outcome = await run.outcome();
      if (outcome.status === "completed" && outcome.outcome.verdict === "pass") {
        passes.recordPass(ctx.runId, { tool_name: input.tool_name, payload: input.payload });
      }
      return { output: renderAdvisorVerdict(outcome) };
    },
  });
}

/**
 * The terminal gate: a `preToolUse` hook on the profile's terminal tool that
 * denies until the advisor has passed this exact submission. It never starts an
 * advisor; a denial reaches the model as a tool error and mutates no state.
 */
export function requireAdvisoryPass(opts: {
  toolName: string;
  passes: AdvisorPassRegistry;
}): HookEntry {
  return {
    event: "preToolUse",
    matcher: { toolName: opts.toolName },
    run: (facts) =>
      opts.passes.hasPass(facts.runId, { tool_name: facts.toolName, payload: facts.input })
        ? { decision: "passthrough" }
        : {
            decision: "deny",
            reason: `advisor has not passed this ${facts.toolName} submission; call ask_advisor with the intended payload first`,
          },
  };
}

function userText(text: string): UserMessage {
  return { role: "user", content: [{ type: "text", text }] };
}

function renderAdvisorVerdict(outcome: AgentOutcome<AdvisorVerdict>): string {
  if (outcome.status !== "completed") return `advisor did not complete (${outcome.status})`;
  return `${outcome.outcome.verdict}: ${outcome.outcome.reason}`;
}

function renderCallerMessages(messages: readonly Message[]): string {
  return messages.map(renderMessage).filter((line) => line.length > 0).join("\n\n");
}

function renderMessage(message: Message): string {
  const parts: string[] = [];
  for (const block of message.content) {
    if (block.type === "text") parts.push(block.text);
    else if (block.type === "tool_use") parts.push(`[calls ${block.name} ${JSON.stringify(block.input)}]`);
    else if (block.type === "tool_result") parts.push(`[result] ${block.content}`);
  }
  const body = parts.join("\n");
  return body.length === 0 ? "" : `## ${message.role}\n${body}`;
}
