import { defineTool, type ToolDefinition } from "eos-agent-sdk";
import { z } from "zod";

/**
 * The coding capability (read/write/edit/exec, bridged to the sandbox daemon).
 * The bridge mechanism is out of this split's scope (spec §7, §16); these are
 * name- and schema-faithful placeholders so every planner/worker profile passes
 * `allowed_tools` validation. Each returns an explicit not-wired error rather
 * than failing silently.
 */
const NOT_WIRED = "sandbox daemon bridge is not wired in this build";

function sandboxStub<I>(name: string, description: string, input: z.ZodType<I>): ToolDefinition {
  return defineTool({
    name,
    description,
    input,
    execute: () => Promise.resolve({ error: NOT_WIRED }),
  });
}

export function sandboxTools(): ToolDefinition[] {
  return [
    sandboxStub(
      "read",
      "Read a file from the sandbox workspace.",
      z.object({
        path: z.string().min(1),
        offset: z.number().int().min(0).optional(),
        limit: z.number().int().positive().optional(),
      }),
    ),
    sandboxStub(
      "multi_read",
      "Read several files from the sandbox workspace.",
      z.object({ paths: z.array(z.string().min(1)).min(1) }),
    ),
    sandboxStub(
      "write",
      "Write a file in the sandbox workspace.",
      z.object({ path: z.string().min(1), content: z.string() }),
    ),
    sandboxStub(
      "edit",
      "Replace a string in a sandbox workspace file.",
      z.object({
        path: z.string().min(1),
        old_string: z.string(),
        new_string: z.string(),
        replace_all: z.boolean().default(false),
      }),
    ),
    sandboxStub(
      "exec_command",
      "Run a shell command in the sandbox workspace.",
      z.object({
        command: z.string().min(1),
        cwd: z.string().optional(),
        timeout_ms: z.number().int().positive().optional(),
      }),
    ),
    sandboxStub(
      "command_stdin",
      "Write to the stdin of a running sandbox command.",
      z.object({ command_id: z.string().min(1), input: z.string() }),
    ),
    sandboxStub(
      "read_command_transcript",
      "Read the transcript of a sandbox command by id.",
      z.object({
        command_id: z.string().min(1),
        offset: z.number().int().min(0).optional(),
        limit: z.number().int().positive().optional(),
      }),
    ),
  ];
}
