import { readFileSync } from "node:fs";
import { join } from "node:path";

import { defineTool, type ToolDefinition } from "eos-agent-sdk";
import { z } from "zod";

/**
 * Read a finished run's recorded messages by run id, paged by line. The SDK
 * writes `<recordsDir>/<runId>/messages.jsonl`; this is the only host seam for
 * inspecting a child run's transcript after it settles.
 */
export function readAgentRun(recordsDir: string): ToolDefinition {
  return defineTool({
    name: "read_agent_run",
    description: "Read a finished agent run's recorded messages by run id.",
    input: z.object({
      run_id: z.string().min(1),
      offset: z.number().int().min(0).default(0),
      limit: z.number().int().positive().max(500).default(100),
    }),
    execute: (input) => {
      const path = join(recordsDir, input.run_id, "messages.jsonl");
      let raw: string;
      try {
        raw = readFileSync(path, "utf8");
      } catch {
        return Promise.resolve({ error: `no records for run "${input.run_id}"` });
      }
      const lines = raw.split("\n").filter((line) => line.length > 0);
      const page = lines.slice(input.offset, input.offset + input.limit);
      return Promise.resolve({
        output: {
          total: lines.length,
          offset: input.offset,
          lines: page,
          eof: input.offset + input.limit >= lines.length,
        },
      });
    },
  });
}
