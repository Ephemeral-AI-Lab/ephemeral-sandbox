import OpenAI from "openai";

import {
  toolUseIdFrom,
  type ContentBlock,
  type Message,
  type ToolSpec,
} from "@eos/contracts";

import type { LlmClient, LlmStreamOptions } from "../client.js";
import {
  OpenAiApiConfigSchema,
  RetryConfigSchema,
  StreamGuardConfigSchema,
  type OpenAiApiConfigInput,
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

/** §5 effort clamp: the responses api has no `max`, `minimal` is native. */
const OPENAI_EFFORT: Record<
  ReasoningEffort,
  "minimal" | "low" | "medium" | "high"
> = {
  minimal: "minimal",
  low: "low",
  medium: "medium",
  high: "high",
  max: "high",
};

/**
 * Project one neutral message onto responses input items: a `message` item
 * for its text parts (if any), plus `function_call`/`function_call_output`
 * items for tool calls and results. Reasoning is provider-managed and
 * dropped on encode (§5 caveat 1).
 */
function encodeMessageItems(
  message: Message,
): OpenAI.Responses.ResponseInputItem[] {
  const textType = message.role === "user" ? "input_text" : "output_text";
  const items: OpenAI.Responses.ResponseInputItem[] = [];
  const textParts: { type: string; text: string }[] = [];
  for (const block of message.content) {
    switch (block.type) {
      case "text":
        textParts.push({ type: textType, text: block.text });
        break;
      case "tool_use":
        items.push({
          type: "function_call",
          call_id: block.tool_use_id,
          name: block.name,
          arguments: JSON.stringify(block.input),
        });
        break;
      case "tool_result":
        items.push({
          type: "function_call_output",
          call_id: block.tool_use_id,
          output: block.content,
        });
        break;
      case "reasoning":
        break;
    }
  }
  if (textParts.length > 0) {
    items.unshift({
      type: "message",
      role: message.role,
      content: textParts,
      // The sdk input-item type wants ids/status on replayed assistant
      // messages; the wire accepts the minimal shape used here.
    } as unknown as OpenAI.Responses.ResponseInputItem);
  }
  return items;
}

function encodeTool(spec: ToolSpec): OpenAI.Responses.Tool {
  const tool: OpenAI.Responses.FunctionTool = {
    type: "function",
    name: spec.name,
    description: spec.description,
    parameters: spec.input_schema,
    // Neutral tool schemas are arbitrary json schema, not strict-mode shaped.
    strict: false,
  };
  if (spec.output_schema === undefined) return tool;
  // §5: output_schema mapped where supported; not yet in the sdk tool type.
  return {
    ...tool,
    output_schema: spec.output_schema,
  } as unknown as OpenAI.Responses.Tool;
}

function encodeToolChoice(
  choice: ToolChoice,
): OpenAI.Responses.ResponseCreateParams["tool_choice"] {
  if (choice === "auto") return "auto";
  if (choice === "any") return "required";
  return { type: "function", name: choice.tool };
}

/** Project a neutral request onto `POST /v1/responses` streaming params. */
export function encodeOpenAiRequest(
  request: LlmRequest,
): OpenAI.Responses.ResponseCreateParamsStreaming {
  const params: OpenAI.Responses.ResponseCreateParamsStreaming = {
    model: request.model,
    input: request.messages.flatMap(encodeMessageItems),
    max_output_tokens: request.max_tokens,
    // Stateless replay: nothing is stored provider-side.
    store: false,
    stream: true,
  };
  if (request.system_prompt !== undefined) {
    params.instructions = request.system_prompt;
  }
  if (request.tools.length > 0) {
    params.tools = request.tools.map(encodeTool);
  }
  if (request.tool_choice !== undefined) {
    params.tool_choice = encodeToolChoice(request.tool_choice);
  }
  if (request.reasoning_effort !== undefined) {
    params.reasoning = { effort: OPENAI_EFFORT[request.reasoning_effort] };
  }
  return params;
}

/**
 * The responses-stream state machine. Text deltas stream from
 * `response.output_text.delta`, reasoning summaries from
 * `response.reasoning_summary_text.delta`, function calls assemble from
 * `response.output_item.added` plus argument deltas keyed by `item_id` and
 * close at `response.function_call_arguments.done`, and the terminus is the
 * completed (or incomplete) response snapshot. Unknown event types are
 * ignored (forward compatibility).
 */
class OpenAiDecoder
  implements StreamDecoder<OpenAI.Responses.ResponseStreamEvent>
{
  completed = false;
  readonly #requestId: string | undefined;
  readonly #items = new Map<
    string,
    { callId: string; name: string; args: string }
  >();
  readonly #tools: ContentBlock[] = [];
  #text = "";
  #reasoning = "";

  constructor(requestId: string | undefined) {
    this.#requestId = requestId;
  }

  *handle(
    event: OpenAI.Responses.ResponseStreamEvent,
  ): Iterable<LlmStreamEvent> {
    switch (event.type) {
      case "response.output_text.delta":
        this.#text += event.delta;
        yield { type: "assistant_text_delta", text: event.delta };
        break;
      case "response.reasoning_summary_text.delta":
        this.#reasoning += event.delta;
        yield { type: "reasoning_delta", text: event.delta };
        break;
      case "response.output_item.added":
        if (event.item.type === "function_call") {
          this.#items.set(event.item.id ?? "", {
            callId: event.item.call_id,
            name: event.item.name,
            args: "",
          });
        }
        break;
      case "response.function_call_arguments.delta": {
        const item = this.#items.get(event.item_id);
        if (item) item.args += event.delta;
        break;
      }
      case "response.function_call_arguments.done": {
        const item = this.#items.get(event.item_id);
        this.#items.delete(event.item_id);
        if (!item) break;
        if (item.callId === "") {
          throw new ProviderError("decode", "function call missing call id", {
            request_id: this.#requestId,
          });
        }
        const toolUseId = toolUseIdFrom(item.callId);
        const input = parseToolArgs(item.args);
        this.#tools.push({
          type: "tool_use",
          tool_use_id: toolUseId,
          name: item.name,
          input,
        });
        yield {
          type: "tool_use_delta",
          tool_use_id: toolUseId,
          name: item.name,
          input,
        };
        break;
      }
      case "response.completed":
      case "response.incomplete":
        this.completed = true;
        yield this.#complete(event.response);
        break;
      case "response.failed":
        throw new ProviderError(
          "decode",
          event.response.error?.message ?? "response failed",
          { request_id: this.#requestId },
        );
      case "error":
        throw new ProviderError("decode", event.message, {
          request_id: this.#requestId,
        });
      default:
        break;
    }
  }

  #complete(response: OpenAI.Responses.Response): LlmStreamEvent {
    const content: ContentBlock[] = [];
    if (this.#reasoning !== "") {
      content.push({ type: "reasoning", text: this.#reasoning });
    }
    if (this.#text !== "") {
      content.push({ type: "text", text: this.#text });
    }
    content.push(...this.#tools);
    // The sdk type marks usage fields required; the wire may omit them and
    // absent fields decode to zero.
    const reported = response.usage as
      | {
          input_tokens?: number;
          output_tokens?: number;
          input_tokens_details?: { cached_tokens?: number };
        }
      | undefined;
    const usage: UsageSnapshot = {
      input_tokens: reported?.input_tokens ?? 0,
      output_tokens: reported?.output_tokens ?? 0,
    };
    const cached = reported?.input_tokens_details?.cached_tokens;
    if (typeof cached === "number") {
      usage.cache_read_input_tokens = cached;
    }
    return {
      type: "assistant_message_complete",
      message: { role: "assistant", content },
      usage,
      stop_reason: this.#deriveStopReason(response),
    };
  }

  /**
   * §5 derivation: function calls present -> `tool_use`; incomplete on
   * `max_output_tokens` -> `max_tokens`; complete -> `end_turn`; anything
   * else passes through verbatim.
   */
  #deriveStopReason(
    response: OpenAI.Responses.Response,
  ): StopReason | undefined {
    if (this.#tools.length > 0) return "tool_use";
    if (response.status === "incomplete") {
      return response.incomplete_details?.reason === "max_output_tokens"
        ? "max_tokens"
        : (response.incomplete_details?.reason ?? "incomplete");
    }
    if (response.status === "completed") return "end_turn";
    return response.status;
  }
}

/** The OpenAI Responses streaming client. */
export class OpenAiResponsesClient implements LlmClient {
  readonly #sdk: OpenAI;
  readonly #retry: RetryConfig;
  readonly #idleTimeoutMs: number;

  constructor(
    config: OpenAiApiConfigInput,
    options: ProviderClientOptions = {},
  ) {
    const { base_url, api_key } = OpenAiApiConfigSchema.parse(config);
    this.#retry = RetryConfigSchema.parse(options.retry ?? {});
    this.#idleTimeoutMs =
      StreamGuardConfigSchema.parse(options.streamGuard ?? {}).idle_timeout_s *
      1000;
    this.#sdk = new OpenAI({
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
    const params = encodeOpenAiRequest(request);
    const attempt = () =>
      runAttempt(
        {
          open: async (signal) => {
            const { data, response } = await this.#sdk.responses
              .create(params, { signal })
              .withResponse();
            return {
              stream: data,
              requestId: response.headers.get("x-request-id") ?? undefined,
            };
          },
          decoder: (requestId) => new OpenAiDecoder(requestId),
        },
        this.#idleTimeoutMs,
        options?.signal,
      );
    return retryStream(this.#retry, attempt, options?.signal);
  }
}
