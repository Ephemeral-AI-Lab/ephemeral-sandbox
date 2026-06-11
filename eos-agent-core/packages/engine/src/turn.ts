import type { ContentBlock, Message, ToolSpec } from "@eos/contracts";
import {
  ProviderError,
  type LlmClient,
  type LlmRequest,
  type ReasoningEffort,
  type StopReason,
  type UsageSnapshot,
} from "@eos/llm-client";
import type {
  AgentEvent,
  PartialReason,
} from "./agent-runtime-handle.js";

import type { Conversation } from "./conversation.js";

/** Per-run provider-call configuration, fixed at `startAgentRun`. */
export interface TurnConfig {
  client: LlmClient;
  model: string;
  systemPrompt?: string;
  maxTokens: number;
  reasoningEffort?: ReasoningEffort;
  /** Evaluated per turn: the executor filters specs by workspace mode. */
  toolSpecs: () => ToolSpec[];
}

/** One successfully completed assistant turn. */
export interface CompletedTurn {
  message: Message;
  usage: UsageSnapshot;
  stop_reason?: StopReason;
}

/** Sum per-turn usage into a run total; cache fields appear once reported. */
export function addUsage(
  total: UsageSnapshot,
  turn: UsageSnapshot,
): UsageSnapshot {
  const sum: UsageSnapshot = {
    input_tokens: total.input_tokens + turn.input_tokens,
    output_tokens: total.output_tokens + turn.output_tokens,
  };
  const cacheRead =
    (total.cache_read_input_tokens ?? 0) + (turn.cache_read_input_tokens ?? 0);
  if (cacheRead > 0) sum.cache_read_input_tokens = cacheRead;
  const cacheCreation =
    (total.cache_creation_input_tokens ?? 0) +
    (turn.cache_creation_input_tokens ?? 0);
  if (cacheCreation > 0) sum.cache_creation_input_tokens = cacheCreation;
  return sum;
}

/**
 * Stream one provider turn: forward each `LlmStreamEvent` as an
 * `AgentEvent` and return the completed message. When the turn dies instead
 * (abort mid-stream, `ProviderError`), accumulated text/reasoning is
 * salvaged to `displayed_messages` only — incomplete `tool_use` blocks are
 * discarded entirely — and the error is rethrown for the loop's catch-all
 * to classify by `signal.aborted`, never by error type.
 */
export async function runAssistantTurn(
  cfg: TurnConfig,
  conversation: Conversation,
  signal: AbortSignal,
  emit: (event: AgentEvent) => void,
): Promise<CompletedTurn> {
  const request: LlmRequest = {
    model: cfg.model,
    messages: [...conversation.llmMessages()],
    system_prompt: cfg.systemPrompt,
    max_tokens: cfg.maxTokens,
    tools: cfg.toolSpecs(),
    reasoning_effort: cfg.reasoningEffort,
  };
  let text = "";
  let reasoning = "";
  try {
    for await (const event of cfg.client.streamMessage(request, { signal })) {
      if (event.type === "assistant_text_delta") text += event.text;
      if (event.type === "reasoning_delta") reasoning += event.text;
      emit(event);
      if (event.type === "assistant_message_complete") {
        return {
          message: event.message,
          usage: event.usage,
          stop_reason: event.stop_reason,
        };
      }
    }
    throw new Error("provider stream ended without assistant completion");
  } catch (error) {
    const reason: PartialReason | undefined = signal.aborted
      ? "interrupted"
      : error instanceof ProviderError
        ? "provider_error"
        : undefined;
    const content: ContentBlock[] = [];
    if (reasoning) content.push({ type: "reasoning", text: reasoning });
    if (text) content.push({ type: "text", text });
    if (reason && content.length > 0) {
      conversation.appendPartialAssistant({ role: "assistant", content }, reason);
    }
    throw error;
  }
}
