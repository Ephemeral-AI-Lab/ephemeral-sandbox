import Anthropic from "@anthropic-ai/sdk";

import {
  toolUseIdFrom,
  type ContentBlock,
  type Message,
  type ToolSpec,
} from "@eos/contracts";

import type { LlmClient, LlmStreamOptions } from "../client.js";
import {
  AnthropicApiConfigSchema,
  RetryConfigSchema,
  StreamGuardConfigSchema,
  type AnthropicApiConfigInput,
  type ProviderClientOptions,
  type RetryConfig,
} from "../config.js";
import { ProviderError } from "../errors.js";
import type { LlmStreamEvent, StopReason } from "../events.js";
import { retryStream } from "../retry.js";
import type {
  LlmRequest,
  ReasoningEffort,
  ToolChoice,
  UsageSnapshot,
} from "../types.js";
import {
  parseToolArgs,
  runAttempt,
  type StreamDecoder,
} from "./attempt.js";

/** §5 effort clamp: the messages api has no `minimal`, `max` is native. */
const ANTHROPIC_EFFORT: Record<
  ReasoningEffort,
  "low" | "medium" | "high" | "max"
> = {
  minimal: "low",
  low: "low",
  medium: "medium",
  high: "high",
  max: "max",
};

function encodeContentBlock(
  block: ContentBlock,
): Anthropic.ContentBlockParam | undefined {
  switch (block.type) {
    case "text":
      return { type: "text", text: block.text };
    case "tool_use":
      return {
        type: "tool_use",
        id: block.tool_use_id,
        name: block.name,
        input: block.input,
      };
    case "tool_result":
      return {
        type: "tool_result",
        tool_use_id: block.tool_use_id,
        content: block.content,
        is_error: block.is_error,
      };
    case "reasoning":
      // Provider-managed: replay of signed thinking blocks arrives with the
      // thinking phase (§5 caveat 1); until then reasoning is dropped.
      return undefined;
  }
}

function encodeMessage(message: Message): Anthropic.MessageParam {
  return {
    role: message.role,
    content: message.content
      .map(encodeContentBlock)
      .filter((block) => block !== undefined),
  };
}

function encodeTool(spec: ToolSpec): Anthropic.Tool {
  // output_schema is dropped: the messages api has no output schema field.
  return {
    name: spec.name,
    description: spec.description,
    input_schema: spec.input_schema as Anthropic.Tool.InputSchema,
  };
}

function encodeToolChoice(choice: ToolChoice): Anthropic.ToolChoice {
  if (choice === "auto") return { type: "auto" };
  if (choice === "any") return { type: "any" };
  return { type: "tool", name: choice.tool };
}

/** Project a neutral request onto `POST /v1/messages` streaming params. */
export function encodeAnthropicRequest(
  request: LlmRequest,
): Anthropic.MessageCreateParamsStreaming {
  const params: Anthropic.MessageCreateParamsStreaming = {
    model: request.model,
    max_tokens: request.max_tokens,
    messages: request.messages.map(encodeMessage),
    stream: true,
  };
  if (request.system_prompt !== undefined) {
    params.system = request.system_prompt;
  }
  if (request.tools.length > 0) {
    params.tools = request.tools.map(encodeTool);
  }
  if (request.tool_choice !== undefined) {
    params.tool_choice = encodeToolChoice(request.tool_choice);
  }
  if (request.reasoning_effort !== undefined) {
    params.output_config = {
      effort: ANTHROPIC_EFFORT[request.reasoning_effort],
    };
  }
  return params;
}

interface BlockAccum {
  blockType: string;
  id: string;
  name: string;
  text: string;
  inputJson: string;
}

type PartialUsage = Partial<
  Record<
    | "input_tokens"
    | "output_tokens"
    | "cache_read_input_tokens"
    | "cache_creation_input_tokens",
    number | null
  >
>;

/**
 * The messages-stream state machine. `message_start` carries initial usage,
 * `message_delta` carries the stop reason and output/cache usage, tool-use
 * blocks assemble across `content_block_start`/`input_json_delta`/
 * `content_block_stop`, and `message_stop` is the terminus. Unknown event
 * and block types are ignored (forward compatibility; revisited when
 * server-side compaction blocks must be preserved).
 */
class AnthropicDecoder implements StreamDecoder<Anthropic.RawMessageStreamEvent> {
  completed = false;
  readonly #requestId: string | undefined;
  readonly #blocks = new Map<number, BlockAccum>();
  readonly #content: ContentBlock[] = [];
  readonly #usage: UsageSnapshot = { input_tokens: 0, output_tokens: 0 };
  #stopReason: StopReason | undefined;

  constructor(requestId: string | undefined) {
    this.#requestId = requestId;
  }

  *handle(event: Anthropic.RawMessageStreamEvent): Iterable<LlmStreamEvent> {
    switch (event.type) {
      case "message_start":
        this.#mergeUsage(event.message.usage);
        break;
      case "content_block_start": {
        const block = event.content_block;
        this.#blocks.set(event.index, {
          blockType: block.type,
          id: "id" in block ? block.id : "",
          name: "name" in block ? block.name : "",
          text: "",
          inputJson: "",
        });
        break;
      }
      case "content_block_delta": {
        const accum = this.#blocks.get(event.index);
        switch (event.delta.type) {
          case "text_delta":
            if (accum) accum.text += event.delta.text;
            yield { type: "assistant_text_delta", text: event.delta.text };
            break;
          case "thinking_delta":
            if (accum) accum.text += event.delta.thinking;
            yield { type: "reasoning_delta", text: event.delta.thinking };
            break;
          case "input_json_delta":
            if (accum) accum.inputJson += event.delta.partial_json;
            break;
          default:
            // signature_delta and friends stay provider-managed until the
            // thinking phase owns reasoning replay.
            break;
        }
        break;
      }
      case "content_block_stop": {
        const accum = this.#blocks.get(event.index);
        this.#blocks.delete(event.index);
        if (!accum) break;
        if (accum.blockType === "text") {
          this.#content.push({ type: "text", text: accum.text });
        } else if (accum.blockType === "thinking") {
          this.#content.push({ type: "reasoning", text: accum.text });
        } else if (accum.blockType === "tool_use") {
          if (accum.id === "") {
            throw new ProviderError("decode", "tool use block missing id", {
              request_id: this.#requestId,
            });
          }
          const toolUseId = toolUseIdFrom(accum.id);
          const input = parseToolArgs(accum.inputJson);
          this.#content.push({
            type: "tool_use",
            tool_use_id: toolUseId,
            name: accum.name,
            input,
          });
          yield {
            type: "tool_use_delta",
            tool_use_id: toolUseId,
            name: accum.name,
            input,
          };
        }
        break;
      }
      case "message_delta":
        if (event.delta.stop_reason) {
          this.#stopReason = event.delta.stop_reason;
        }
        this.#mergeUsage(event.usage);
        break;
      case "message_stop":
        this.completed = true;
        yield {
          type: "assistant_message_complete",
          message: { role: "assistant", content: [...this.#content] },
          usage: { ...this.#usage },
          stop_reason: this.#stopReason,
        };
        break;
      default:
        break;
    }
  }

  #mergeUsage(usage: PartialUsage): void {
    if (typeof usage.input_tokens === "number") {
      this.#usage.input_tokens = usage.input_tokens;
    }
    if (typeof usage.output_tokens === "number") {
      this.#usage.output_tokens = usage.output_tokens;
    }
    if (typeof usage.cache_read_input_tokens === "number") {
      this.#usage.cache_read_input_tokens = usage.cache_read_input_tokens;
    }
    if (typeof usage.cache_creation_input_tokens === "number") {
      this.#usage.cache_creation_input_tokens =
        usage.cache_creation_input_tokens;
    }
  }
}

/** The Anthropic Messages streaming client. */
export class AnthropicApiClient implements LlmClient {
  readonly #sdk: Anthropic;
  readonly #retry: RetryConfig;
  readonly #idleTimeoutMs: number;

  constructor(
    config: AnthropicApiConfigInput,
    options: ProviderClientOptions = {},
  ) {
    const { base_url, api_key } = AnthropicApiConfigSchema.parse(config);
    this.#retry = RetryConfigSchema.parse(options.retry ?? {});
    this.#idleTimeoutMs =
      StreamGuardConfigSchema.parse(options.streamGuard ?? {}).idle_timeout_s *
      1000;
    this.#sdk = new Anthropic({
      // Credentials are always explicit; sdk env-var fallback is not used.
      apiKey: api_key.expose(),
      baseURL: base_url,
      // The retry gate is the single retry-policy owner.
      maxRetries: 0,
      // The sdk parse-failure path echoes frame content through its logger;
      // provider frames must stay out of logs.
      logLevel: "off",
      ...(options.fetch !== undefined ? { fetch: options.fetch } : {}),
    });
  }

  streamMessage(
    request: LlmRequest,
    options?: LlmStreamOptions,
  ): AsyncIterable<LlmStreamEvent> {
    const params = encodeAnthropicRequest(request);
    const attempt = () =>
      runAttempt(
        {
          open: async (signal) => {
            const { data, response } = await this.#sdk.messages
              .create(params, { signal })
              .withResponse();
            return {
              stream: data,
              requestId: response.headers.get("request-id") ?? undefined,
            };
          },
          decoder: (requestId) => new AnthropicDecoder(requestId),
        },
        this.#idleTimeoutMs,
        options?.signal,
      );
    return retryStream(this.#retry, attempt, options?.signal);
  }
}
