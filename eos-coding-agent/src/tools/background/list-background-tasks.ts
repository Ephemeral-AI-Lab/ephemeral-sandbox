import { defineTool, type BackgroundTaskRow } from "eos-agent-sdk";
import { z } from "zod";

/** One row per active task: its `{type, id}` tag, title, and start time. */
export function renderBackgroundTaskRows(rows: readonly BackgroundTaskRow[]): string {
  if (rows.length === 0) return "no active background tasks";
  return rows
    .map((row) => `${row.tag.type}/${row.tag.id} — ${row.title} (started ${String(row.startedAt)})`)
    .join("\n");
}

export const listBackgroundTasks = defineTool({
  name: "list_background_tasks",
  description: "List this run's active background tasks.",
  input: z.object({}),
  execute: (_input, ctx) =>
    Promise.resolve({ output: renderBackgroundTaskRows(ctx.backgroundTaskSupervisor.list()) }),
});
