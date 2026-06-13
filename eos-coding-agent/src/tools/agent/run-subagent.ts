import {
  defineTool,
  type AgentOutcome,
  type AgentRunId,
  type BackgroundTaskOutcome,
  type ToolDefinition,
} from "eos-agent-sdk";
import { z } from "zod";

import type { AgentFactory } from "../../agents/agent-factory.js";

/**
 * Start another configured agent by name, gated on the caller profile's
 * `subagents` allow-list (the input enum). Foreground (`wait`) returns the
 * outcome; background registers exactly one task whose `onCompletion` is the
 * only completion publisher.
 */
export function runSubagent(
  agents: AgentFactory,
  subagents: readonly [string, ...string[]],
): ToolDefinition {
  return defineTool({
    name: "run_subagent",
    description: "Run another configured agent.",
    input: z.object({
      agent_name: z.enum(subagents),
      prompt: z.string().min(1),
      wait: z.boolean().default(true),
    }),
    execute: async (input, ctx) => {
      const child = agents.create(input.agent_name);
      const run = child.start({
        messages: [{ role: "user", content: [{ type: "text", text: input.prompt }] }],
      });
      ctx.signal.addEventListener("abort", () => {
        run.interrupt();
      });

      if (input.wait) {
        return { output: renderAgentOutcome(await run.outcome()) };
      }

      ctx.backgroundTaskSupervisor.register({
        tag: { type: "subagent", id: run.runId },
        title: `${input.agent_name}: ${input.prompt.slice(0, 80)}`,
        cancel: () => {
          run.interrupt();
        },
        done: run.outcome().then(toBackgroundTaskOutcome),
        onCompletion: (out, { notifier }) => {
          notifier.publish(renderSubagentCompletion(input.agent_name, run.runId, out), {
            key: `subagent:${run.runId}`,
          });
        },
      });
      return { output: `subagent started: ${run.runId}` };
    },
  });
}

function toBackgroundTaskOutcome(outcome: AgentOutcome): BackgroundTaskOutcome {
  if (outcome.status === "completed") return { status: "success", outcome: outcome.outcome.slice(0, 280) };
  if (outcome.status === "cancelled") return { status: "cancelled", outcome: "cancelled" };
  return { status: "failed", outcome: outcome.error.message };
}

function renderAgentOutcome(outcome: AgentOutcome): string {
  if (outcome.status === "completed") return outcome.outcome;
  if (outcome.status === "cancelled") return "subagent cancelled";
  return `subagent failed: ${outcome.error.message}`;
}

function renderSubagentCompletion(
  agentName: string,
  runId: AgentRunId,
  out: BackgroundTaskOutcome,
): string {
  return `subagent ${agentName} (${runId}) ${out.status}: ${out.outcome}`;
}
