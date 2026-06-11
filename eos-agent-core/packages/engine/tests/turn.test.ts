import { describe, expect, it } from "vitest";

import type { AgentEvent } from "../src/agent-runtime-handle.js";
import { ProviderError, type UsageSnapshot } from "@eos/llm-client";

import { Conversation } from "../src/conversation.js";
import { addUsage, runAssistantTurn, type TurnConfig } from "../src/turn.js";
import {
  MockLlmClient,
  assistantMessage,
  complete,
  deferred,
  failingTurn,
  hangingTurn,
  must,
  reasoningDelta,
  scriptedTurn,
  textBlock,
  textDelta,
  userText,
  type ScriptedTurn,
} from "./support.js";

function setup(turns: ScriptedTurn[]): {
  client: MockLlmClient;
  cfg: TurnConfig;
  conversation: Conversation;
  emitted: AgentEvent[];
  emit: (event: AgentEvent) => void;
} {
  const client = new MockLlmClient(turns);
  const cfg: TurnConfig = {
    client,
    model: "mock-model",
    systemPrompt: "be terse",
    maxTokens: 256,
    toolSpecs: () => [{ name: "echo", description: "echo", input_schema: {} }],
  };
  const conversation = new Conversation([userText("hi")]);
  const emitted: AgentEvent[] = [];
  return {
    client,
    cfg,
    conversation,
    emitted,
    emit: (event) => {
      emitted.push(event);
    },
  };
}

describe("runAssistantTurn", () => {
  it("builds the request from llmMessages() and the fixed turn config", async () => {
    const reply = assistantMessage(textBlock("hello"));
    const { client, cfg, conversation, emit } = setup([
      scriptedTurn([complete(reply)]),
    ]);
    const signal = new AbortController().signal;
    await runAssistantTurn(cfg, conversation, signal, emit);
    expect(client.requests).toHaveLength(1);
    expect(must(client.requests.at(0))).toEqual({
      model: "mock-model",
      messages: [userText("hi")],
      system_prompt: "be terse",
      max_tokens: 256,
      tools: cfg.toolSpecs(),
      reasoning_effort: undefined,
    });
  });

  it("forwards every delta as an AgentEvent and returns the completed turn", async () => {
    const reply = assistantMessage(textBlock("hello"));
    const { cfg, conversation, emitted, emit } = setup([
      scriptedTurn([
        reasoningDelta("hmm"),
        textDelta("hel"),
        textDelta("lo"),
        complete(reply, "end_turn"),
      ]),
    ]);
    const turn = await runAssistantTurn(
      cfg,
      conversation,
      new AbortController().signal,
      emit,
    );
    expect(emitted.map((event) => event.type)).toEqual([
      "reasoning_delta",
      "assistant_text_delta",
      "assistant_text_delta",
      "assistant_message_complete",
    ]);
    expect(turn.message).toEqual(reply);
    expect(turn.stop_reason).toBe("end_turn");
    expect(turn.usage).toEqual({ input_tokens: 10, output_tokens: 5 });
  });

  it("salvages text and reasoning to displayed-only on abort, then rethrows", async () => {
    const streamed = deferred();
    const { cfg, conversation, emit } = setup([
      hangingTurn([reasoningDelta("plan"), textDelta("Hello, wo")], streamed),
    ]);
    const controller = new AbortController();
    const pending = runAssistantTurn(cfg, conversation, controller.signal, emit);
    await streamed.promise;
    controller.abort();
    await expect(pending).rejects.toThrow();
    expect(conversation.llmMessages()).toEqual([userText("hi")]);
    const salvaged = must(conversation.displayedMessages().at(-1));
    expect(salvaged.partial).toBe("interrupted");
    expect(salvaged.message).toEqual(
      assistantMessage(
        { type: "reasoning", text: "plan" },
        { type: "text", text: "Hello, wo" },
      ),
    );
  });

  it("salvages pre-error deltas as provider_error on a ProviderError", async () => {
    const { cfg, conversation, emit } = setup([
      failingTurn(
        [textDelta("partial out")],
        new ProviderError("server", "upstream died", { status_code: 500 }),
      ),
    ]);
    const pending = runAssistantTurn(
      cfg,
      conversation,
      new AbortController().signal,
      emit,
    );
    await expect(pending).rejects.toBeInstanceOf(ProviderError);
    const salvaged = must(conversation.displayedMessages().at(-1));
    expect(salvaged.partial).toBe("provider_error");
    expect(salvaged.message).toEqual(assistantMessage(textBlock("partial out")));
    expect(conversation.llmMessages()).toEqual([userText("hi")]);
  });

  it("treats a stream that ends without completion as an internal error, no salvage", async () => {
    const { cfg, conversation, emit } = setup([scriptedTurn([textDelta("oops")])]);
    const pending = runAssistantTurn(
      cfg,
      conversation,
      new AbortController().signal,
      emit,
    );
    await expect(pending).rejects.toThrow(
      "provider stream ended without assistant completion",
    );
    expect(conversation.displayedMessages()).toHaveLength(1);
    expect(conversation.llmMessages()).toEqual([userText("hi")]);
  });
});

describe("addUsage", () => {
  it("sums tokens and surfaces cache fields only once reported", () => {
    const base: UsageSnapshot = { input_tokens: 10, output_tokens: 5 };
    const summed = addUsage(base, {
      input_tokens: 7,
      output_tokens: 3,
      cache_read_input_tokens: 11,
    });
    expect(summed).toEqual({
      input_tokens: 17,
      output_tokens: 8,
      cache_read_input_tokens: 11,
    });
    expect(
      addUsage(base, base),
      "cache fields stay absent when never reported",
    ).toEqual({ input_tokens: 20, output_tokens: 10 });
  });
});
