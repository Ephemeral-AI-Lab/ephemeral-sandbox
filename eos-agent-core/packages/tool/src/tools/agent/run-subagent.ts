import type { JsonValue } from "@eos/contracts";
import {
  RUN_FINISHED_DISPOSE_REASON,
  type AgentRunOutcome,
  type BackgroundSupervisor,
  type SessionOutcome,
} from "@eos/engine";
import { z } from "zod";

import type { ToolDefinition } from "../../contract.js";
import { defineTool } from "../../define.js";
import type { AgentRunCalls, AgentToolUserMessage } from "./index.js";

function userText(text: string): AgentToolUserMessage {
  return { role: "user", content: [{ type: "text", text }] };
}

const RunSubagentInputSchema = z.object({
  agent_name: z.string().min(1),
  prompt: z.string().min(1),
});

export function runSubagentTool(
  calls: AgentRunCalls,
  supervisor: BackgroundSupervisor,
): ToolDefinition {
  return defineTool({
    name: "run_subagent",
    description:
      "Start the named agent as a detached background run on the given prompt. Returns its run_id immediately; the outcome arrives later as a session_settled notification.",
    input: RunSubagentInputSchema,
    execute: (input) => {
      // Deliberately NO signal: a detached run gets a fresh abort root and
      // never dies with the caller's turn. Cancellation reaches it only
      // through the §8 disposal cascade or cancel_background_session.
      const subagent = calls.startRun({
        agentName: input.agent_name,
        initialMessages: [userText(input.prompt)],
      });
      supervisor.register(
        { type: "subagent", id: subagent.runId },
        {
          settled: subagent.handle.outcome.then(mapSubagentOutcome),
          cancel: async (reason) => {
            // §8 fixed reasons: the engine's dispose-on-finish marks the
            // disposal cascade; any other cancel reaches a subagent only
            // through cancel_background_session (model-initiated).
            subagent.handle.interrupt(
              reason === RUN_FINISHED_DISPOSE_REASON
                ? "caller_disposed"
                : "model_cancelled",
            );
            await subagent.handle.outcome;
          },
        },
      );
      return Promise.resolve({ content: { run_id: subagent.runId } });
    },
  });
}

function mapSubagentOutcome(outcome: AgentRunOutcome): SessionOutcome {
  switch (outcome.status) {
    case "completed":
      return { status: "completed", summary: submissionSummary(outcome.submission) };
    case "cancelled":
      return { status: "cancelled", summary: outcome.reason };
    case "failed":
      return { status: "failed", summary: outcome.failure.message };
  }
}

function submissionSummary(submission: JsonValue | undefined): string {
  if (
    typeof submission === "object" &&
    submission !== null &&
    !Array.isArray(submission) &&
    typeof submission.summary === "string"
  ) {
    return submission.summary;
  }
  return submission === undefined ? "completed" : JSON.stringify(submission);
}
