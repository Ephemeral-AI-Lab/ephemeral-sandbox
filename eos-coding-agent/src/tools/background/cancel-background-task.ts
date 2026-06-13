import { defineTool } from "eos-agent-sdk";
import { z } from "zod";

export const cancelBackgroundTask = defineTool({
  name: "cancel_background_task",
  description: "Cancel one active background task in this run, addressed by its {type, id} tag.",
  input: z.object({
    type: z.string().min(1),
    id: z.string().min(1),
    reason: z.string().min(1),
  }),
  execute: async (input, ctx) => ({
    output: (await ctx.backgroundTaskSupervisor.cancel(
      { type: input.type, id: input.id },
      input.reason,
    ))
      ? "cancelled"
      : "not found",
  }),
});
