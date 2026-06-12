import type {
  AgentRunId,
  DelegatePursuitInput,
  PursuitHandle,
} from "@eos/contracts";
import { DelegatePursuitInputSchema } from "@eos/contracts";
import type { BackgroundSessionSupervisor } from "@eos/background";

import type { ToolDefinition } from "../../contract.js";
import { defineTool } from "../../define.js";
import { DESCRIPTION } from "../description_prompts/delegate_pursuit_prompt.js";

/** The family's name universe: `delegate_pursuit`, alone (§2.18). */
export const PURSUIT_TOOL_NAMES = ["delegate_pursuit"] as const;

/**
 * The pursuit family over one bound function plus the per-run supervisor
 * (§2.18). There is no `cancel_pursuit` and no read/query tool this
 * round: cancellation rides `cancel_background_session` through the
 * registered handle, and the read surface is deferred.
 */
export function pursuitTools(
  delegate: (
    input: DelegatePursuitInput,
    parent: AgentRunId,
  ) => Promise<PursuitHandle>,
  supervisor: BackgroundSessionSupervisor,
): ToolDefinition[] {
  return [
    defineTool({
      name: "delegate_pursuit",
      description: DESCRIPTION,
      input: DelegatePursuitInputSchema,
      execute: async (input, ctx) => {
        // One open pursuit per run: running or settled-but-undelivered.
        const open = supervisor
          .listBackgroundSessions()
          .some((session) => session.type === "pursuit");
        if (open) {
          return {
            content:
              "a delegated pursuit is already open for this run; wait for its session_settled notification or cancel it first",
            isError: true,
          };
        }
        const pursuit = await delegate(input, ctx.meta.run.run_id);
        const description =
          input.pursuit_goal.split("\n", 1)[0] ?? input.pursuit_goal;
        // Registration precedes the tool result, exactly the subagent
        // pattern: the submission guard covers the pursuit before the
        // model's next token.
        supervisor.registerBackgroundSession(
          { type: "pursuit", id: pursuit.pursuit_id },
          {
            settled: pursuit.settle().then((terminal) => ({
              status:
                terminal.status === "Success"
                  ? ("completed" as const)
                  : terminal.status === "Cancelled"
                    ? ("cancelled" as const)
                    : ("failed" as const),
              summary: terminal.summary,
            })),
            cancel: (reason) => pursuit.cancel(reason),
            describe: () => description,
          },
        );
        return { content: { pursuit_id: pursuit.pursuit_id } };
      },
    }),
  ];
}
