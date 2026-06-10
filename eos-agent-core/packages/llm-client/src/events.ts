import type { JsonObject, Message, ToolUseId } from "@eos/contracts";

import type { UsageSnapshot } from "./types.js";

/**
 * A parsed provider stop reason. Known values are named; any other provider
 * string passes through verbatim (parse-don't-validate).
 */
export type StopReason =
  | "end_turn"
  | "max_tokens"
  | "tool_use"
  | "stop_sequence"
  | (string & {});

/**
 * A single normalized event from a streaming model invocation.
 *
 * The three delta variants are "visible output" for the retry gate;
 * `assistant_message_complete` is the success terminus and iteration ends
 * after it.
 */
export type LlmStreamEvent =
  | {
      /** Incremental assistant text. */
      type: "assistant_text_delta";
      text: string;
    }
  | {
      /** Incremental model reasoning. */
      type: "reasoning_delta";
      text: string;
    }
  | {
      /**
       * A fully-assembled tool call, emitted at the block's close so the
       * engine can begin executing it early. Malformed argument json yields
       * an empty input.
       */
      type: "tool_use_delta";
      tool_use_id: ToolUseId;
      name: string;
      input: JsonObject;
    }
  | {
      /** The completed assistant message. */
      type: "assistant_message_complete";
      message: Message;
      usage: UsageSnapshot;
      stop_reason?: StopReason;
    };
