import { z } from "zod";

import { ToolUseIdSchema } from "./ids.js";
import { JsonObjectSchema } from "./json.js";

/**
 * The default completion token cap used when an agent profile does not provide
 * a narrower model request limit.
 */
export const DEFAULT_MAX_TOKENS = 32768;

/**
 * The role of a conversation message. There is deliberately no `system` role:
 * the system prompt is a request field, never a message, so `"system"` fails
 * to parse.
 */
export const MessageRoleSchema = z.enum(["user", "assistant"]);
export type MessageRole = z.infer<typeof MessageRoleSchema>;

/**
 * A single content block within a `Message`.
 *
 * Extension policy: this union grows additively (new variants, new optional
 * fields) and each future variant is owned by the phase that introduces it.
 */
export const ContentBlockSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("text"),
    text: z.string(),
  }),
  z.object({
    type: z.literal("tool_use"),
    tool_use_id: ToolUseIdSchema,
    name: z.string(),
    input: JsonObjectSchema,
  }),
  z.object({
    type: z.literal("tool_result"),
    tool_use_id: ToolUseIdSchema,
    content: z.string(),
    is_error: z.boolean().default(false),
  }),
  z.object({
    type: z.literal("reasoning"),
    text: z.string(),
  }),
]);
export type ContentBlock = z.infer<typeof ContentBlockSchema>;

/** A single user or assistant message. */
export const MessageSchema = z.object({
  role: MessageRoleSchema,
  content: z.array(ContentBlockSchema).default([]),
});
export type Message = z.infer<typeof MessageSchema>;

/** A neutral tool declaration sent to the model. */
export const ToolSpecSchema = z.object({
  name: z.string(),
  description: z.string(),
  input_schema: JsonObjectSchema,
  output_schema: JsonObjectSchema.optional(),
});
export type ToolSpec = z.infer<typeof ToolSpecSchema>;

/** Construct a user message from raw text. */
export function fromUserText(text: string): Message {
  return { role: "user", content: [{ type: "text", text }] };
}

/** Concatenated text blocks, excluding reasoning. */
export function assistantText(message: Message): string {
  return message.content
    .filter((block) => block.type === "text")
    .map((block) => block.text)
    .join("");
}

/** Concatenated reasoning blocks. */
export function reasoningText(message: Message): string {
  return message.content
    .filter((block) => block.type === "reasoning")
    .map((block) => block.text)
    .join("");
}

/** The tool-use blocks in the message, in order. */
export function toolUses(
  message: Message,
): Extract<ContentBlock, { type: "tool_use" }>[] {
  return message.content.filter((block) => block.type === "tool_use");
}
