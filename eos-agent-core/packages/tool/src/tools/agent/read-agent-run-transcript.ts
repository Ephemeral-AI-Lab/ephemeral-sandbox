import { agentRunIdFrom } from "@eos/contracts";
import { z } from "zod";

import type { ToolDefinition } from "../../contract.js";
import { defineTool } from "../../define.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import type { AgentRunCalls } from "./index.js";

const DEFAULT_READ_BYTES = 65_536;
const MAX_READ_BYTES = 262_144;

const ReadTranscriptInputSchema = z.object({
  run_id: z.string().min(1),
  /** Byte offset to resume from; 0 reads from the start. */
  offset: z.number().int().min(0).default(0),
  max_bytes: z.number().int().positive().max(MAX_READ_BYTES).default(DEFAULT_READ_BYTES),
});

export function readAgentRunTranscriptTool(calls: AgentRunCalls): ToolDefinition {
  return defineTool({
    name: "read_agent_run_transcript",
    description: descriptionPrompt("read_agent_run_transcript"),
    input: ReadTranscriptInputSchema,
    execute: async (input) => {
      const path = calls.transcriptPathOf(agentRunIdFrom(input.run_id));
      if (path === undefined) {
        return { content: `unknown agent run: ${input.run_id}`, isError: true };
      }
      const read = await calls.readTranscriptFile(path, input.offset, input.max_bytes);
      return {
        content: { transcript: read.data, next_offset: read.next_offset, eof: read.eof },
      };
    },
  });
}
