import { describe, expect, it } from "vitest";

import {
  ContentBlockSchema,
  DEFAULT_MAX_TOKENS,
  MessageRoleSchema,
  MessageSchema,
  ToolCallResultSchema,
  ToolSpecSchema,
  ToolUseIdSchema,
  agentRunIdFrom,
  assistantText,
  mintAgentRunId,
  mintBackgroundTaskId,
  reasoningText,
  toolUseIdFrom,
  toolUses,
  type Message,
} from "../../src/contracts/index.js";

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
    expect(
      MessageRoleSchema.safeParse("system").success,
      "role schema",
    ).toBe(false);
    expect(
      MessageSchema.safeParse({ role: "system", content: [] }).success,
      "message schema",
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
    expect("metadata" in block, "metadata is stripped").toBe(false);
    expect("is_terminal" in block, "is_terminal is stripped").toBe(false);
  });

  it("rejects the cut thinking alias and system_notification variant", () => {
    expect(
      ContentBlockSchema.safeParse({ type: "thinking", text: "x" }).success,
      "thinking alias",
    ).toBe(false);
    expect(
      ContentBlockSchema.safeParse({ type: "system_notification", text: "x" })
        .success,
      "system_notification variant",
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

describe("agent run and background task ids", () => {
  it("mints dashed uuidv4 run ids and adopts existing ones", () => {
    const minted = mintAgentRunId();
    expect(minted).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(agentRunIdFrom(minted)).toBe(minted);
    expect(() => agentRunIdFrom("")).toThrow();
  });

  it("mints non-empty task ids", () => {
    expect(mintBackgroundTaskId().length).toBeGreaterThan(0);
  });
});

describe("tool call results", () => {
  it("round-trips structured content and keeps is_error required", () => {
    const result = ToolCallResultSchema.parse({
      tool_use_id: "toolu_1",
      content: { summary: "done", payload: { ok: true } },
      is_error: false,
      is_terminal: true,
      tool_start_time: 1,
      tool_end_time: 2,
      metadata: { hook_warnings: ["w"] },
    });
    expect(ToolCallResultSchema.parse(JSON.parse(JSON.stringify(result)))).toEqual(
      result,
    );
    expect(
      ToolCallResultSchema.safeParse({
        tool_use_id: "toolu_1",
        content: "x",
        is_terminal: false,
        tool_start_time: 1,
        tool_end_time: 2,
      }).success,
      "is_error has no default; the executor must normalize it",
    ).toBe(false);
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
