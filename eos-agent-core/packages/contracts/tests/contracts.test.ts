import { describe, expect, it } from "vitest";

import {
  ContentBlockSchema,
  DEFAULT_MAX_TOKENS,
  MessageRoleSchema,
  MessageSchema,
  ToolSpecSchema,
  ToolUseIdSchema,
  assistantText,
  fromUserText,
  reasoningText,
  toolUseIdFrom,
  toolUses,
  type Message,
} from "../src/index.js";

describe("message dtos", () => {
  it("round-trips a message through json", () => {
    const parsed = MessageSchema.parse({
      role: "assistant",
      content: [
        { type: "reasoning", text: "think" },
        { type: "text", text: "hello" },
        { type: "tool_use", tool_use_id: "toolu_1", name: "read", input: { path: "a" } },
      ],
    });
    const reparsed = MessageSchema.parse(JSON.parse(JSON.stringify(parsed)));
    expect(reparsed).toEqual(parsed);
    expect(reparsed.content).toHaveLength(3);
  });

  it("rejects the system role", () => {
    expect(MessageRoleSchema.safeParse("system").success).toBe(false);
    expect(
      MessageSchema.safeParse({ role: "system", content: [] }).success,
    ).toBe(false);
  });

  it("defaults message content to an empty array", () => {
    expect(MessageSchema.parse({ role: "user" }).content).toEqual([]);
  });

  it("defaults tool_result is_error to false and round-trips it", () => {
    const block = ContentBlockSchema.parse({
      type: "tool_result",
      tool_use_id: "toolu_1",
      content: "ok",
    });
    expect(block).toEqual({
      type: "tool_result",
      tool_use_id: "toolu_1",
      content: "ok",
      is_error: false,
    });
  });

  it("strips the cut rust tool_result fields", () => {
    const block = ContentBlockSchema.parse({
      type: "tool_result",
      tool_use_id: "toolu_1",
      content: "ok",
      metadata: { secret: "nope" },
      is_terminal: true,
    });
    expect("metadata" in block).toBe(false);
    expect("is_terminal" in block).toBe(false);
  });

  it("rejects the cut thinking alias and system_notification variant", () => {
    expect(
      ContentBlockSchema.safeParse({ type: "thinking", text: "x" }).success,
    ).toBe(false);
    expect(
      ContentBlockSchema.safeParse({ type: "system_notification", text: "x" })
        .success,
    ).toBe(false);
  });

  it("parses a tool spec with and without output_schema", () => {
    const spec = ToolSpecSchema.parse({
      name: "read",
      description: "read a file",
      input_schema: { type: "object" },
    });
    expect(spec.output_schema).toBeUndefined();
    const withOutput = ToolSpecSchema.parse({
      ...spec,
      output_schema: { type: "string" },
    });
    expect(withOutput.output_schema).toEqual({ type: "string" });
  });

  it("exposes the default max tokens", () => {
    expect(DEFAULT_MAX_TOKENS).toBe(32768);
  });
});

describe("tool use ids", () => {
  it("adopts provider-assigned ids and rejects empty ones", () => {
    expect(toolUseIdFrom("toolu_01")).toBe("toolu_01");
    expect(() => toolUseIdFrom("")).toThrow();
    expect(ToolUseIdSchema.safeParse("").success).toBe(false);
  });
});

describe("message helpers", () => {
  const message: Message = {
    role: "assistant",
    content: [
      { type: "reasoning", text: "let me " },
      { type: "reasoning", text: "think" },
      { type: "text", text: "Hello" },
      { type: "text", text: " world" },
      {
        type: "tool_use",
        tool_use_id: toolUseIdFrom("toolu_1"),
        name: "read",
        input: {},
      },
    ],
  };

  it("builds a user message from text", () => {
    expect(fromUserText("hi")).toEqual({
      role: "user",
      content: [{ type: "text", text: "hi" }],
    });
  });

  it("concatenates text and reasoning separately", () => {
    expect(assistantText(message)).toBe("Hello world");
    expect(reasoningText(message)).toBe("let me think");
  });

  it("lists tool uses in order", () => {
    const uses = toolUses(message);
    expect(uses).toHaveLength(1);
    expect(uses[0]?.name).toBe("read");
  });
});
