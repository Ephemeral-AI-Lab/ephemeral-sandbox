import type { BackgroundSessionSupervisor } from "@eos/background";
import { z } from "zod";

import type { ToolDefinition } from "../../contract.js";
import { defineTool } from "../../define.js";
import { descriptionPrompt } from "../description_prompts/index.js";

// `type` is an open string this phase; it narrows to the session-kind
// enum as the spawning families land.
const CancelInputSchema = z.object({
  type: z.string().min(1),
  id: z.string().min(1),
  reason: z.string().optional(),
});

/** Cancel by native `(type, id)` ref - no minted session-id namespace. */
export function cancelBackgroundSessionTool(
  supervisor: BackgroundSessionSupervisor,
): ToolDefinition {
  return defineTool({
    name: "cancel_background_session",
    description: descriptionPrompt("cancel_background_session"),
    input: CancelInputSchema,
    execute: async ({ type, id, reason }) => {
      const row = supervisor
        .listBackgroundSessions()
        .find((candidate) => candidate.type === type && candidate.id === id);
      if (!row) {
        return { content: `no background session ${type}:${id}`, isError: true };
      }
      if (row.status !== "running") {
        return {
          content: `background session ${type}:${id} already settled (${row.status}); nothing to cancel`,
        };
      }
      const cancelled = await supervisor.cancelBackgroundSession(
        { type, id },
        reason ?? "cancelled by request",
      );
      return {
        content: cancelled
          ? `background session ${type}:${id} cancelled`
          : `background session ${type}:${id} already settled; nothing to cancel`,
      };
    },
  });
}
