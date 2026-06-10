import {
  DEFAULT_MAX_TOKENS,
  type Message,
  type ToolSpec,
} from "@eos/contracts";

/** How the model should choose among the offered tools. */
export type ToolChoice = "auto" | "any" | { tool: string };

/**
 * Provider reasoning-effort hint. The neutral set is the union of both
 * providers' vocabularies; each encoder clamps to its provider's range.
 */
export type ReasoningEffort = "minimal" | "low" | "medium" | "high" | "max";

/**
 * Token usage reported by a model provider. The optional cache fields feed
 * future cache-aware context management; absent input/output fields decode
 * to zero.
 */
export interface UsageSnapshot {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

/** Total accounted tokens (`input + output`). */
export function totalTokens(usage: UsageSnapshot): number {
  return usage.input_tokens + usage.output_tokens;
}

/** A neutral model invocation request. */
export interface LlmRequest {
  /** Opaque provider model key. */
  model: string;
  /** The conversation so far. */
  messages: Message[];
  /** The system prompt; a request field, never a message. */
  system_prompt?: string;
  /** Maximum completion tokens. */
  max_tokens: number;
  /** The tools offered to the model. */
  tools: ToolSpec[];
  /** The tool-choice control, when forced. */
  tool_choice?: ToolChoice;
  /** Optional reasoning-effort hint. */
  reasoning_effort?: ReasoningEffort;
}

export type LlmRequestInit = Partial<LlmRequest> & Pick<LlmRequest, "model">;

/** Build a request with defaults: empty history, no tools, default cap. */
export function buildLlmRequest(init: LlmRequestInit): LlmRequest {
  return {
    messages: [],
    tools: [],
    max_tokens: DEFAULT_MAX_TOKENS,
    ...init,
  };
}
