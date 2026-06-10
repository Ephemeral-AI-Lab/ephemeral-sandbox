import { describe, expect, it } from "vitest";

import {
  assistantText,
  reasoningText,
  toolUseIdFrom,
  toolUses,
} from "@eos/contracts";

import { ProviderError } from "../src/errors.js";
import { encodeAnthropicRequest, AnthropicApiClient } from "../src/providers/anthropic.js";
import { buildLlmRequest } from "../src/types.js";
import {
  collect,
  collectUntilError,
  errorResponse,
  fetchStub,
  fixture,
  hangingSseResponse,
  sseResponse,
} from "./support.js";

const NO_RETRY = { max_retries: 0, base_delay_s: 0, max_delay_s: 0 };

function client(
  stub: ReturnType<typeof fetchStub>,
  options: ConstructorParameters<typeof AnthropicApiClient>[1] = {},
): AnthropicApiClient {
  return new AnthropicApiClient(
    { api_key: "test-key" },
    { retry: NO_RETRY, fetch: stub.fetch, ...options },
  );
}

describe("anthropic golden decode (real sdk parser via injected fetch)", () => {
  it("decodes the full fixture into the normalized event sequence", async () => {
    const stub = fetchStub([
      () => sseResponse(fixture("./fixtures/anthropic/full.sse"), {
        "request-id": "req-test",
      }),
    ]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "claude-test" })),
    );

    expect(events).toHaveLength(5);
    expect(events[0]).toEqual({ type: "reasoning_delta", text: "Let me think" });
    expect(events[1]).toEqual({ type: "assistant_text_delta", text: "Hello" });
    expect(events[2]).toEqual({ type: "assistant_text_delta", text: " world" });
    expect(events[3]).toEqual({
      type: "tool_use_delta",
      tool_use_id: "toolu_01",
      name: "read_file",
      input: { path: "foo.txt" },
    });

    const complete = events[4];
    if (complete.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    expect(complete.usage).toEqual({ input_tokens: 10, output_tokens: 15 });
    expect(complete.stop_reason).toBe("tool_use");
    expect(complete.message.content).toHaveLength(3);
    expect(assistantText(complete.message)).toBe("Hello world");
    expect(reasoningText(complete.message)).toBe("Let me think");
    expect(toolUses(complete.message)).toHaveLength(1);
  });

  it("maps thinking deltas and blocks to reasoning", async () => {
    const sse = [
      'event: content_block_start',
      'data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}',
      "",
      "event: content_block_delta",
      'data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reasoning step"}}',
      "",
      "event: content_block_stop",
      'data: {"type":"content_block_stop","index":0}',
      "",
      "event: message_stop",
      'data: {"type":"message_stop"}',
      "",
    ].join("\n");
    const stub = fetchStub([() => sseResponse(sse)]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "m" })),
    );
    expect(events[0]).toEqual({ type: "reasoning_delta", text: "reasoning step" });
    const complete = events.at(-1);
    if (complete?.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    expect(complete.message.content).toEqual([
      { type: "reasoning", text: "reasoning step" },
    ]);
  });

  it("carries cache usage fields through to the completion", async () => {
    const sse = [
      "event: message_start",
      'data: {"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":100,"output_tokens":1,"cache_read_input_tokens":40,"cache_creation_input_tokens":9}}}',
      "",
      "event: message_delta",
      'data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}',
      "",
      "event: message_stop",
      'data: {"type":"message_stop"}',
      "",
    ].join("\n");
    const stub = fetchStub([() => sseResponse(sse)]);
    const events = await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "m" })),
    );
    const complete = events.at(-1);
    if (complete?.type !== "assistant_message_complete") {
      throw new Error("expected a completion event");
    }
    // An empty assistant message is legal (no-tool-use turn).
    expect(complete.message.content).toEqual([]);
    expect(complete.usage).toEqual({
      input_tokens: 100,
      output_tokens: 6,
      cache_read_input_tokens: 40,
      cache_creation_input_tokens: 9,
    });
    expect(complete.stop_reason).toBe("end_turn");
  });

  it("ends a malformed frame as a non-retryable decode error with the request id", async () => {
    const stub = fetchStub([
      () => sseResponse(fixture("./fixtures/anthropic/malformed.sse"), {
        "request-id": "req-test",
      }),
    ]);
    const { error } = await collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "m" })),
    );
    expect(error).toBeInstanceOf(ProviderError);
    const provider = error as ProviderError;
    expect(provider.kind).toBe("decode");
    expect(provider.truncated).toBe(false);
    expect(provider.request_id).toBe("req-test");
    expect(stub.calls).toHaveLength(1);
  });

  it("treats a stream without message_stop as a retryable truncated stream", async () => {
    const sse = [
      "event: message_start",
      'data: {"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":3,"output_tokens":1}}}',
      "",
    ].join("\n");
    const stub = fetchStub([
      () => sseResponse(sse, { "request-id": "req-trunc" }),
    ]);
    const truncatedClient = new AnthropicApiClient(
      { api_key: "k" },
      {
        retry: { ...NO_RETRY, max_retries: 1 },
        fetch: stub.fetch,
      },
    );
    const { error } = await collectUntilError(
      truncatedClient.streamMessage(buildLlmRequest({ model: "m" })),
    );
    const provider = error as ProviderError;
    expect(provider.kind).toBe("decode");
    expect(provider.truncated).toBe(true);
    expect(provider.request_id).toBe("req-trunc");
    // Truncated streams are retryable pre-visible: 1 + max_retries attempts.
    expect(stub.calls).toHaveLength(2);
  });

  it("surfaces an in-stream error event as decode with the provider message", async () => {
    const sse = [
      "event: content_block_start",
      'data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}',
      "",
      "event: error",
      'data: {"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}',
      "",
    ].join("\n");
    const stub = fetchStub([() => sseResponse(sse)]);
    const { error } = await collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "m" })),
    );
    const provider = error as ProviderError;
    expect(provider.kind).toBe("decode");
    expect(provider.message).toContain("overloaded");
  });
});

describe("anthropic transport reliability", () => {
  it("aborts an idle stream as a transport failure", async () => {
    const stub = fetchStub([
      (init) =>
        hangingSseResponse(
          'event: message_start\ndata: {"type":"message_start","message":{"usage":{"input_tokens":1}}}\n\n',
          init,
        ),
    ]);
    const idleClient = client(stub, {
      retry: NO_RETRY,
      streamGuard: { idle_timeout_s: 0.05 },
      fetch: stub.fetch,
    });
    const { error } = await collectUntilError(
      idleClient.streamMessage(buildLlmRequest({ model: "m" })),
    );
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).kind).toBe("transport");
  });

  it("maps http failures through the status table with retry-after capture", async () => {
    const stub = fetchStub([
      () => errorResponse(429, { "retry-after": "9", "request-id": "req_429" }),
    ]);
    const { error } = await collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "m" })),
    );
    const provider = error as ProviderError;
    expect(provider.kind).toBe("rate_limit");
    expect(provider.status_code).toBe(429);
    expect(provider.retry_after_s).toBe(9);
    expect(provider.request_id).toBe("req_429");
  });

  it("owns retries alone: sdk maxRetries is zero", async () => {
    const stub = fetchStub([() => errorResponse(503)]);
    const gateClient = new AnthropicApiClient(
      { api_key: "k" },
      { retry: { ...NO_RETRY, max_retries: 1 }, fetch: stub.fetch },
    );
    const { error } = await collectUntilError(
      gateClient.streamMessage(buildLlmRequest({ model: "m" })),
    );
    expect((error as ProviderError).kind).toBe("server");
    // 1 + max_retries fetches; any sdk-internal retry would inflate this.
    expect(stub.calls).toHaveLength(2);
  });

  it("rethrows the abort error as-is when the caller cancels mid-stream", async () => {
    const stub = fetchStub([
      (init) =>
        hangingSseResponse(
          'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
          init,
        ),
    ]);
    const controller = new AbortController();
    const pending = collectUntilError(
      client(stub).streamMessage(buildLlmRequest({ model: "m" }), {
        signal: controller.signal,
      }),
    );
    setTimeout(() => { controller.abort(); }, 20);
    const { error } = await pending;
    expect(controller.signal.aborted).toBe(true);
    // Classified by signal.aborted, never by error type.
    expect(error).toBe(controller.signal.reason);
  });
});

describe("anthropic encode projection (§5 column)", () => {
  it("projects the full request surface", () => {
    const request = buildLlmRequest({
      model: "claude-test",
      system_prompt: "be terse",
      max_tokens: 64,
      messages: [
        {
          role: "assistant",
          content: [
            { type: "reasoning", text: "private" },
            { type: "text", text: "hi" },
          ],
        },
        {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: toolUseIdFrom("toolu_9"),
              content: "ok",
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
      reasoning_effort: "minimal",
    });
    const params = encodeAnthropicRequest(request);

    expect(params.stream).toBe(true);
    expect(params.system).toBe("be terse");
    expect(params.max_tokens).toBe(64);
    // Reasoning blocks are dropped on encode (provider-managed).
    expect(params.messages[0]?.content).toEqual([{ type: "text", text: "hi" }]);
    expect(params.messages[1]?.content).toEqual([
      {
        type: "tool_result",
        tool_use_id: "toolu_9",
        content: "ok",
        is_error: false,
      },
    ]);
    // output_schema is dropped; input_schema is preserved.
    expect(params.tools).toEqual([
      {
        name: "read_file",
        description: "Read a file",
        input_schema: { type: "object" },
      },
    ]);
    expect(params.tool_choice).toEqual({ type: "tool", name: "read_file" });
    // minimal clamps to low.
    expect(params.output_config).toEqual({ effort: "low" });
  });

  it("maps tool_choice auto and any", () => {
    const auto = encodeAnthropicRequest(
      buildLlmRequest({ model: "m", tool_choice: "auto" }),
    );
    expect(auto.tool_choice).toEqual({ type: "auto" });
    const any = encodeAnthropicRequest(
      buildLlmRequest({ model: "m", tool_choice: "any" }),
    );
    expect(any.tool_choice).toEqual({ type: "any" });
  });

  it("clamps the effort vocabulary per the §5 table", () => {
    const effortOf = (effort: "minimal" | "low" | "medium" | "high" | "max") =>
      encodeAnthropicRequest(
        buildLlmRequest({ model: "m", reasoning_effort: effort }),
      ).output_config?.effort;
    expect(effortOf("minimal")).toBe("low");
    expect(effortOf("low")).toBe("low");
    expect(effortOf("medium")).toBe("medium");
    expect(effortOf("high")).toBe("high");
    expect(effortOf("max")).toBe("max");
  });

  it("omits optional fields and sends explicit credentials on the wire", async () => {
    const stub = fetchStub([
      () =>
        sseResponse(
          'event: message_stop\ndata: {"type":"message_stop"}\n\n',
        ),
    ]);
    await collect(
      client(stub).streamMessage(buildLlmRequest({ model: "claude-test" })),
    );
    expect(stub.calls).toHaveLength(1);
    const call = stub.calls[0];
    expect(call.url).toBe("https://api.anthropic.com/v1/messages");
    const body = call.body as Record<string, unknown>;
    expect(body.model).toBe("claude-test");
    expect(body.stream).toBe(true);
    expect(body.messages).toEqual([]);
    expect(body).not.toHaveProperty("system");
    expect(body).not.toHaveProperty("tools");
    expect(body).not.toHaveProperty("tool_choice");
    expect(body).not.toHaveProperty("output_config");
    const headers = new Headers(call.init?.headers);
    expect(headers.get("x-api-key")).toBe("test-key");
    expect(headers.get("anthropic-version")).toBeTruthy();
  });
});
