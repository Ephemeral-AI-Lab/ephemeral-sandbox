import { z } from "zod";

/**
 * Identifier for a single tool use. Provider-assigned by the model stream, so
 * there is deliberately no local mint helper - minting one would be a bug.
 */
export const ToolUseIdSchema = z.string().min(1).brand<"ToolUseId">();

export type ToolUseId = z.infer<typeof ToolUseIdSchema>;

/** Adopt a provider-assigned id. Rejects the empty string. */
export function toolUseIdFrom(raw: string): ToolUseId {
  return ToolUseIdSchema.parse(raw);
}

/**
 * Identifier for one agent run. Minted by whoever starts the run as a
 * dashed UUIDv4.
 */
const AgentRunIdSchema = z.string().min(1).brand<"AgentRunId">();

export type AgentRunId = z.infer<typeof AgentRunIdSchema>;

/** Adopt an existing run id (restart, parent reference). Rejects "". */
export function agentRunIdFrom(raw: string): AgentRunId {
  return AgentRunIdSchema.parse(raw);
}

/** Mint a fresh run id. */
export function mintAgentRunId(): AgentRunId {
  return AgentRunIdSchema.parse(crypto.randomUUID());
}

/**
 * Identifier for one registered background task. Minted by the run's
 * supervisor at `register`; hosts only ever receive one.
 */
const BackgroundTaskIdSchema = z
  .string()
  .min(1)
  .brand<"BackgroundTaskId">();

export type BackgroundTaskId = z.infer<typeof BackgroundTaskIdSchema>;

/** Mint a fresh task id. */
export function mintBackgroundTaskId(): BackgroundTaskId {
  return BackgroundTaskIdSchema.parse(crypto.randomUUID());
}
