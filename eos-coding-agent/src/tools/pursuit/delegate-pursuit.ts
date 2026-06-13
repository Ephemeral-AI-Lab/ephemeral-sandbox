import { defineTool, type BackgroundTaskOutcome, type ToolDefinition } from "eos-agent-sdk";
import {
  CreatePursuitInputSchema,
  type PursuitAgents,
  type PursuitService,
  type PursuitSettlement,
} from "@eos/pursuit";

const DELEGATE_PURSUIT_DESCRIPTION =
  "Delegate a multi-leg coding pursuit. It registers a background task you can watch with " +
  "list_background_tasks and stop with cancel_background_task on the returned pursuit id.";

/**
 * Provider-authored, named off the configured workflow name (delegate_pursuit,
 * delegate_pursuit_staging, …). Starts the pursuit, registers it as a background
 * task tagged by the delegated domain id, and lets SDK run-end disposal cancel a
 * still-running pursuit when the delegating run terminates.
 */
export function delegatePursuit(
  name: string,
  service: PursuitService,
  agents: PursuitAgents,
): ToolDefinition {
  return defineTool({
    name: `delegate_${name}`,
    description: DELEGATE_PURSUIT_DESCRIPTION,
    input: CreatePursuitInputSchema,
    execute: async (input, ctx) => {
      const handle = await service.createPursuit(input, { agents });
      ctx.backgroundTaskSupervisor.register({
        tag: { type: name, id: handle.pursuitId },
        title: handle.title,
        cancel: (reason) => {
          return handle.cancel(reason);
        },
        done: handle.done.then(toBackgroundTaskOutcome),
        onCompletion: (out, { notifier }) => {
          notifier.publish(`${name} ${out.status}: ${out.outcome}`, {
            key: `${name}:${handle.pursuitId}`,
          });
        },
      });
      return { output: `pursuit delegated: ${handle.pursuitId} — ${handle.title}` };
    },
  });
}

function toBackgroundTaskOutcome(settlement: PursuitSettlement): BackgroundTaskOutcome {
  const status =
    settlement.status === "Success"
      ? "success"
      : settlement.status === "Cancelled"
        ? "cancelled"
        : "failed";
  return { status, outcome: settlement.summary };
}
