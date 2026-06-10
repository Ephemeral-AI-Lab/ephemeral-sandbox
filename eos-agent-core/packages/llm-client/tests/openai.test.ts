import { describe, expect, it } from "vitest";

import { toolUseIdFrom } from "@eos/contracts";

import { ProviderError } from "../src/errors.js";
import {
  AnthropicApiClient,
} from "../src/providers/anthropic.js";
import {
  OpenAiResponsesClient,
  encodeOpenAiRequest,
} from "../src/providers/openai.js";
import { buildLlmRequest } from "../src/types.js";
import {
  collect,
  collectUntilError,
  fetchStub,
  fixture,
  sseResponse,
} from "./support.js";

const NO_RETRY = { max_retries: 0, base_delay_s: 0, max_delay_s: 0 };

function client(stub: ReturnType<typeof fetchStub>): OpenAiResponsesClient {
  return new OpenAiResponsesClient(
    { api_key: "test-key" },
    { retry: NO_RETRY, fetch: stub.fetch },
  );
}

function completedSse(response: Record<string, unknown>): string {
  return `data: ${JSON.stringify({ type: "response.completed", response })}\n\n`;
}

describe("openai golden decode (real sdk parser via injected fetch)", () => {
  it("decodes the full fixture into the normalized event sequence", async () => {
    const stub = fetchStub([
      () =>
        sseResponse(fixture("./fixtures/openai/full.sse"), {
          "x-request-id": "req-test",
        }),
    ]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt-test" })),
    );

    expect(events).toHaveLength(3);
    expect(events[0]).toEqual({ type: "assistant_text_delta", text: "Hi" });
    expect(events[1]).toEqual({
      type: "tool_use_delta",
      tool_use_id: "call_9",
      name: "read_file",
      input: { path: "foo.txt" },
    });
    const complete = events[2];
    if (complete.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    expect(complete.usage).toEqual({ input_tokens: 5, output_tokens: 4 });
    expect(complete.stop_reason).toBe("tool_use");
    expect(complete.message.content).toHaveLength(2);
  });

  it("is variant-substitutable with the anthropic text+tool path", async () => {
    const openAiStub = fetchStub([
      () => sseResponse(fixture("./fixtures/openai/full.sse")),
    ]);
    const openAiEvents = await collect(
      client(openAiStub).streamMessage(buildLlmRequest({ model: "gpt" })),
    );

    const anthropicStub = fetchStub([
      () => sseResponse(fixture("./fixtures/anthropic/text_tool.sse")),
    ]);
    const anthropicEvents = await collect(
      new AnthropicApiClient(
        { api_key: "k" },
        { retry: NO_RETRY, fetch: anthropicStub.fetch },
      ).streamMessage(buildLlmRequest({ model: "claude" })),
    );

    expect(openAiEvents.map((event) => event.type)).toEqual(
      anthropicEvents.map((event) => event.type),
    );
  });

  it("maps reasoning summary deltas to reasoning", async () => {
    const sse = [
      'data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"thinking "}',
      "",
      'data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"hard"}',
      "",
      completedSse({ id: "r", status: "completed", usage: { input_tokens: 1, output_tokens: 2 } }),
    ].join("\n");
    const stub = fetchStub([() => sseResponse(sse)]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt" })),
    );
    expect(events[0]).toEqual({ type: "reasoning_delta", text: "thinking " });
    expect(events[1]).toEqual({ type: "reasoning_delta", text: "hard" });
    const complete = events.at(-1);
    if (complete?.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    expect(complete.message.content).toEqual([
      { type: "reasoning", text: "thinking hard" },
    ]);
  });

  it("derives stop reasons per the §5 table", async () => {
    const run = async (sse: string) => {
      const stub = fetchStub([() => sseResponse(sse)]);
      const events = await collect(
        client(stub).streamMessage(buildLlmRequest({ model: "gpt" })),
      );
      const complete = events.at(-1);
      if (complete?.type !== "assistant_message_complete") {
        throw new Error("expected a completion event");
      }
      return complete;
    };

    // complete, no calls -> end_turn
    const endTurn = await run(
      completedSse({ id: "r", status: "completed", usage: { input_tokens: 1, output_tokens: 1 } }),
    );
    expect(endTurn.stop_reason).toBe("end_turn");
    expect(endTurn.message.content).toEqual([]);

    // incomplete on max_output_tokens -> max_tokens
    const maxTokens = await run(
      `data: ${JSON.stringify({
        type: "response.incomplete",
        response: {
          id: "r",
          status: "incomplete",
          incomplete_details: { reason: "max_output_tokens" },
          usage: { input_tokens: 1, output_tokens: 1 },
        },
      })}\n\n`,
    );
    expect(maxTokens.stop_reason).toBe("max_tokens");

    // any other incomplete reason passes through verbatim
    const filtered = await run(
      `data: ${JSON.stringify({
        type: "response.incomplete",
        response: {
          id: "r",
          status: "incomplete",
          incomplete_details: { reason: "content_filter" },
        },
      })}\n\n`,
    );
    expect(filtered.stop_reason).toBe("content_filter");
  });

  it("maps cached input tokens into cache_read_input_tokens", async () => {
    const stub = fetchStub([
      () =>
        sseResponse(
          completedSse({
            id: "r",
            status: "completed",
            usage: {
              input_tokens: 50,
              output_tokens: 7,
              input_tokens_details: { cached_tokens: 30 },
            },
          }),
        ),
    ]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt" })),
    );
    const complete = events.at(-1);
    if (complete?.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    expect(complete.usage).toEqual({
      input_tokens: 50,
      output_tokens: 7,
      cache_read_input_tokens: 30,
    });
  });

  it("treats a stream without a terminal response event as truncated", async () => {
    const sse =
      'data: {"type":"response.output_text.delta","delta":"partial"}\n\n';
    const stub = fetchStub([
      () => sseResponse(sse, { "x-request-id": "req-trunc" }),
    ]);
    const { events, error } = await collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt" })),
    );
    expect(events).toEqual([
      { type: "assistant_text_delta", text: "partial" },
    ]);
    const provider = error as ProviderError;
    expect(provider.kind).toBe("decode");
    expect(provider.truncated).toBe(true);
    expect(provider.request_id).toBe("req-trunc");
  });

  it("surfaces response.failed as a decode error with the provider message", async () => {
    const sse = `data: ${JSON.stringify({
      type: "response.failed",
      response: {
        id: "r",
        status: "failed",
        error: { code: "server_error", message: "model exploded" },
      },
    })}\n\n`;
    const stub = fetchStub([() => sseResponse(sse)]);
    const { error } = await collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt" })),
    );
    const provider = error as ProviderError;
    expect(provider.kind).toBe("decode");
    expect(provider.message).toContain("model exploded");
  });
});

describe("openai encode projection (§5 column)", () => {
  it("projects the full request surface", () => {
    const params = encodeOpenAiRequest(
      buildLlmRequest({
        model: "gpt-test",
        system_prompt: "be terse",
        max_tokens: 256,
        messages: [
          { role: "user", content: [{ type: "text", text: "hi" }] },
          {
            role: "assistant",
            content: [
              { type: "reasoning", text: "private" },
              { type: "text", text: "hello" },
              {
                type: "tool_use",
                tool_use_id: toolUseIdFrom("call_1"),
                name: "read_file",
                input: { path: "foo.txt" },
              },
            ],
          },
          {
            role: "user",
            content: [
              {
                type: "tool_result",
                tool_use_id: toolUseIdFrom("call_1"),
                content: "file body",
                is_error: false,
              },
            ],
          },
        ],
        tools: [
          {
            name: "read_file",
            description: "Read a file",
            input_schema: { type: "object" },
            output_schema: { type: "string" },
          },
        ],
        tool_choice: { tool: "read_file" },
        reasoning_effort: "max",
      }),
    );

    expect(params.stream).toBe(true);
    expect(params.store).toBe(false);
    expect(params.instructions).toBe("be terse");
    expect(params.max_output_tokens).toBe(256);
    expect(params.input).toEqual([
      {
        type: "message",
        role: "user",
        content: [{ type: "input_text", text: "hi" }],
      },
      {
        // Reasoning blocks are dropped on encode (provider-managed).
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: "hello" }],
      },
      {
        type: "function_call",
        call_id: "call_1",
        name: "read_file",
        arguments: '{"path":"foo.txt"}',
      },
      {
        type: "function_call_output",
        call_id: "call_1",
        output: "file body",
      },
    ]);
    // output_schema is mapped for openai; schemas are not strict-mode shaped.
    expect(params.tools).toEqual([
      {
        type: "function",
        name: "read_file",
        description: "Read a file",
        parameters: { type: "object" },
        strict: false,
        output_schema: { type: "string" },
      },
    ]);
    expect(params.tool_choice).toEqual({ type: "function", name: "read_file" });
    // max clamps to high.
    expect(params.reasoning).toEqual({ effort: "high" });
  });

  it("maps tool_choice auto and any", () => {
    expect(
      encodeOpenAiRequest(buildLlmRequest({ model: "m", tool_choice: "auto" }))
        .tool_choice,
    ).toBe("auto");
    expect(
      encodeOpenAiRequest(buildLlmRequest({ model: "m", tool_choice: "any" }))
        .tool_choice,
    ).toBe("required");
  });

  it("clamps the effort vocabulary per the §5 table", () => {
    const effortOf = (effort: "minimal" | "low" | "medium" | "high" | "max") =>
      encodeOpenAiRequest(
        buildLlmRequest({ model: "m", reasoning_effort: effort }),
      ).reasoning?.effort;
    expect(effortOf("minimal")).toBe("minimal");
    expect(effortOf("low")).toBe("low");
    expect(effortOf("medium")).toBe("medium");
    expect(effortOf("high")).toBe("high");
    expect(effortOf("max")).toBe("high");
  });

  it("sends bearer credentials to the responses endpoint", async () => {
    const stub = fetchStub([
      () =>
        sseResponse(
          completedSse({ id: "r", status: "completed", usage: { input_tokens: 0, output_tokens: 0 } }),
        ),
    ]);
    await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "gpt-test" })),
    );
    expect(stub.calls).toHaveLength(1);
    const call = stub.calls[0];
    expect(call.url).toBe("https://api.openai.com/v1/responses");
    const headers = new Headers(call.init?.headers);
    expect(headers.get("authorization")).toBe("Bearer test-key");
    const body = call.body as Record<string, unknown>;
    expect(body.model).toBe("gpt-test");
    expect(body.stream).toBe(true);
    expect(body.store).toBe(false);
    expect(body).not.toHaveProperty("instructions");
    expect(body).not.toHaveProperty("tools");
    expect(body).not.toHaveProperty("reasoning");
  });
});
