import { DEFAULT_MAX_TOKENS, type Message } from "@eos/contracts";
import type { LlmClient, ReasoningEffort } from "@eos/llm-client";
import type { LoopObserver, NotificationInbox } from "@eos/notification";
import { RunHandle, type AgentRunHandle } from "./agent-runtime-handle.js";
import type { BackgroundSessionSupervisor } from "@eos/background";

import { runAgentLoop } from "./agent-loop.js";
import { Conversation } from "./conversation.js";
import type { ToolExecutor } from "./tool-executor.js";

export { RUN_FINISHED_DISPOSE_REASON } from "./agent-loop.js";
export type {
  AgentEvent,
  AgentRunFailure,
  AgentRunHandle,
  AgentRunOutcome,
  DisplayedMessage,
  PartialReason,
} from "./agent-runtime-handle.js";
export type { ToolExecutor, ToolUseBlock } from "./tool-executor.js";

/** Loop-turn budget when the caller does not pass `maxTurns`. */
const DEFAULT_MAX_TURNS = 32;

/** In-process input of `startAgentRun` (camelCase; never serialized). */
export interface StartAgentRunInput {
  /** Already-configured provider client (DI boundary). */
  llmClient: LlmClient;
  /** The one injected tool port (DI boundary); specs re-read per turn. */
  tools: ToolExecutor;
  model: string;
  /** A request field, never a message. */
  systemPrompt?: string;
  /** Seed history; a restart passes a prior outcome's `llm`. Non-empty. */
  initialMessages: Message[];
  /** Default `DEFAULT_MAX_TOKENS`. */
  maxTokens?: number;
  reasoningEffort?: ReasoningEffort;
  /** Provider-call budget; default 32. */
  maxTurns?: number;
  /** Optional parent scope; an external abort ≡ `interrupt()`. */
  signal?: AbortSignal;
  /**
   * Drained at loop boundaries below steers. Pass the instance the
   * supervisor publishes settlements to; the loop itself publishes each
   * result's `metadata.hook_contexts` entry here (Phase 04.5 decision 11).
   */
  notifications?: NotificationInbox;
  /**
   * Background-session lifecycle = loop lifecycle: backs the auto-wait
   * gate and is disposed on every finish. A supervisor implies `notifications` (it
   * publishes settlements there).
   */
  background?: BackgroundSessionSupervisor;
  /**
   * Loop-lifecycle announcement port (Phase 04.9): awaited after every
   * committed assistant turn; `idleStarted`/`idleEnded` bracket each
   * auto-wait park. Absent means today's behavior exactly.
   */
  observer?: LoopObserver;
}

/**
 * Start one agent run as a detached loop and return its handle. The loop
 * never throws: every exit resolves `handle.outcome` exactly once and ends
 * `handle.events` with `run_finished`.
 */
export function startAgentRun(input: StartAgentRunInput): AgentRunHandle {
  if (input.initialMessages.length === 0) {
    throw new TypeError("startAgentRun requires non-empty initialMessages");
  }
  const handle = new RunHandle(input.signal);
  void runAgentLoop({
    handle,
    conversation: new Conversation(input.initialMessages),
    tools: input.tools,
    maxTurns: input.maxTurns ?? DEFAULT_MAX_TURNS,
    notifications: input.notifications,
    background: input.background,
    observer: input.observer,
    turnConfig: {
      client: input.llmClient,
      model: input.model,
      systemPrompt: input.systemPrompt,
      maxTokens: input.maxTokens ?? DEFAULT_MAX_TOKENS,
      reasoningEffort: input.reasoningEffort,
      toolSpecs: () => input.tools.specs(),
    },
  });
  return handle;
}
