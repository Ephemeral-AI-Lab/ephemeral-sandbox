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
export const AgentRunIdSchema = z.string().min(1).brand<"AgentRunId">();

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
 * Identifier for a provisioned sandbox. Assigned by the sandbox runtime, so
 * adopt-only - minting one here would invent a sandbox that does not exist.
 */
export const SandboxIdSchema = z.string().min(1).brand<"SandboxId">();

export type SandboxId = z.infer<typeof SandboxIdSchema>;

/** Adopt a runtime-assigned sandbox id. Rejects "". */
export function sandboxIdFrom(raw: string): SandboxId {
  return SandboxIdSchema.parse(raw);
}
