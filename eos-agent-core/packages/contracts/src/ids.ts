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
