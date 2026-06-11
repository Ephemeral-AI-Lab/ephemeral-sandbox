import type { ContentBlock, Message } from "@eos/contracts";
import type {
  DisplayedMessage,
  PartialReason,
} from "./agent-runtime-handle.js";

/** A `tool_result` content block, the unit `appendToolResults` wraps. */
export type ToolResultBlock = Extract<ContentBlock, { type: "tool_result" }>;

/**
 * The dual transcript: `displayed_messages` for users, `llm_messages` for
 * the provider. Every append writes both lists in one call (single-writer
 * rule); the lists diverge only by declared policy — partial assistant
 * output is displayed-only, and a future compaction phase rewrites
 * `llm_messages` while `displayed_messages` stays append-only.
 * `llmMessages()` is the only history source for a provider request.
 */
export class Conversation {
  #displayed: DisplayedMessage[] = [];
  #llm: Message[] = [];
  #seq = 0;

  constructor(initial: readonly Message[]) {
    for (const message of initial) this.#appendBoth(message);
  }

  /** Initial and steered user input. */
  appendUser(message: Message): void {
    this.#appendBoth(message);
  }

  /** A completed assistant turn. */
  appendAssistant(message: Message): void {
    this.#appendBoth(message);
  }

  /** One batch's results as a single user message, in `tool_use` order. */
  appendToolResults(blocks: ToolResultBlock[]): void {
    this.#appendBoth({ role: "user", content: blocks });
  }

  /** Salvaged partial assistant output; never reaches `llm_messages`. */
  appendPartialAssistant(partial: Message, reason: PartialReason): void {
    this.#displayed.push(this.#displayedEntry(partial, reason));
  }

  /** The ONLY history source for an `LlmRequest`. */
  llmMessages(): readonly Message[] {
    return this.#llm;
  }

  displayedMessages(): readonly DisplayedMessage[] {
    return this.#displayed;
  }

  #appendBoth(message: Message): void {
    this.#displayed.push(this.#displayedEntry(message));
    this.#llm.push(message);
  }

  #displayedEntry(message: Message, partial?: PartialReason): DisplayedMessage {
    const entry: DisplayedMessage = {
      seq: this.#seq,
      created_at: new Date().toISOString(),
      message,
    };
    this.#seq += 1;
    if (partial) entry.partial = partial;
    return entry;
  }
}
