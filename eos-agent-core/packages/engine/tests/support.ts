import { expect } from "vitest";

import {
  fromUserText,
  toolUseIdFrom,
  type ContentBlock,
  type JsonObject,
  type JsonValue,
  type Message,
  type ToolCallResult,
} from "@eos/contracts";
import type {
  LlmClient,
  LlmRequest,
  LlmStreamEvent,
  LlmStreamOptions,
  StopReason,
  UsageSnapshot,
} from "@eos/llm-client";

import type { SessionOutcome } from "../src/background/session.js";
import type { BackgroundSupervisor } from "../src/background/supervisor.js";
import type { AgentEvent } from "../src/events.js";
import type { NotificationInbox } from "../src/notification-inbox.js";
import {
  startAgentRun,
  type AgentRunHandle,
  type AgentRunOutcome,
  type LoopObserver,
  type ToolExecutor,
  type TurnFacts,
} from "../src/index.js";

// --- scripted provider client ----------------------------------------------

/** One scripted provider turn; receives the request and the run's signal. */
export type ScriptedTurn = (
  request: LlmRequest,
  signal: AbortSignal | undefined,
) => AsyncIterable<LlmStreamEvent>;

/** In-process `LlmClient` double: one script per provider call, in order. */
export class MockLlmClient implements LlmClient {
  readonly requests: LlmRequest[] = [];
  readonly #turns: ScriptedTurn[];

  constructor(turns: ScriptedTurn[]) {
    this.#turns = turns;
  }

  streamMessage(
    request: LlmRequest,
    options?: LlmStreamOptions,
  ): AsyncIterable<LlmStreamEvent> {
    const script = this.#turns.at(this.requests.length);
    this.requests.push(request);
    if (!script) {
      throw new Error(`unscripted provider call ${String(this.requests.length)}`);
    }
    return script(request, options?.signal);
  }
}

/** A turn that yields the given events, a microtask apart, then completes. */
export function scriptedTurn(events: LlmStreamEvent[]): ScriptedTurn {
  return async function* () {
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
  };
}

/** A turn that yields events, then throws `error` mid-stream. */
export function failingTurn(
  events: LlmStreamEvent[],
  error: Error,
): ScriptedTurn {
  return async function* () {
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
    throw error;
  };
}

/**
 * A turn that yields events, resolves `streamed`, then hangs until the
 * run's signal aborts — like a live socket killed mid-token.
 */
export function hangingTurn(
  events: LlmStreamEvent[],
  streamed: Deferred,
): ScriptedTurn {
  return async function* (_request, signal) {
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
    streamed.resolve();
    await rejectOnAbort(signal);
  };
}

/** A turn that resolves `started`, waits for `release`, then yields. */
export function gatedTurn(
  started: Deferred,
  release: Promise<void>,
  events: LlmStreamEvent[],
): ScriptedTurn {
  return async function* () {
    started.resolve();
    await release;
    for (const event of events) {
      yield event;
    }
  };
}

// --- event and message builders ---------------------------------------------

export const USAGE: UsageSnapshot = { input_tokens: 10, output_tokens: 5 };

export function textDelta(text: string): LlmStreamEvent {
  return { type: "assistant_text_delta", text };
}

export function reasoningDelta(text: string): LlmStreamEvent {
  return { type: "reasoning_delta", text };
}

export function toolUseDelta(
  id: string,
  name: string,
  input: JsonObject = {},
): LlmStreamEvent {
  return { type: "tool_use_delta", tool_use_id: toolUseIdFrom(id), name, input };
}

export function complete(
  message: Message,
  stop_reason: StopReason = "end_turn",
  usage: UsageSnapshot = USAGE,
): LlmStreamEvent {
  return { type: "assistant_message_complete", message, usage, stop_reason };
}

export function textBlock(text: string): ContentBlock {
  return { type: "text", text };
}

export function toolUseBlock(
  id: string,
  name: string,
  input: JsonObject = {},
): Extract<ContentBlock, { type: "tool_use" }> {
  return { type: "tool_use", tool_use_id: toolUseIdFrom(id), name, input };
}

export function toolResultBlock(
  id: string,
  content: string,
  isError = false,
): Extract<ContentBlock, { type: "tool_result" }> {
  return {
    type: "tool_result",
    tool_use_id: toolUseIdFrom(id),
    content,
    is_error: isError,
  };
}

export function assistantMessage(...content: ContentBlock[]): Message {
  return { role: "assistant", content };
}

export const userText = fromUserText;

// --- scripted tool executor ----------------------------------------------------

/** What a scripted tool yields; the executor fills the call facts. */
export interface ScriptedToolResult {
  content: JsonValue;
  is_error?: boolean;
  is_terminal?: boolean;
  metadata?: JsonObject;
}

export type ScriptedToolHandler = (
  input: JsonObject,
  signal: AbortSignal,
) => Promise<ScriptedToolResult>;

/**
 * A minimal in-test `ToolExecutor`: sequential dispatch (concurrency is
 * `@eos/tool`'s concern), abort-aware so a hanging tool settles the batch
 * with whatever finished — the engine's normalization fills the rest.
 */
export function scriptedExecutor(
  ...tools: [string, ScriptedToolHandler][]
): ToolExecutor {
  const byName = new Map(tools);
  return {
    specs: () =>
      [...byName.keys()].map((name) => ({
        name,
        description: name,
        input_schema: {},
      })),
    async executeBatch(calls, signal, emit) {
      const results: ToolCallResult[] = [];
      for (const call of calls) {
        if (signal.aborted) break;
        emit({
          type: "tool_execution_started",
          tool_use_id: call.tool_use_id,
          name: call.name,
          input: call.input,
        });
        const handler = byName.get(call.name);
        const startedAt = Date.now();
        const output = handler
          ? await settleOrAbort(handler(call.input, signal), signal)
          : { content: `tool not found: ${call.name}`, is_error: true };
        if (isAborted(signal)) break;
        const result: ToolCallResult = {
          tool_use_id: call.tool_use_id,
          content: output.content,
          is_error: output.is_error ?? false,
          is_terminal: output.is_terminal ?? false,
          tool_start_time: startedAt,
          tool_end_time: Date.now(),
          ...(output.metadata !== undefined && { metadata: output.metadata }),
        };
        results.push(result);
        emit({
          type: "tool_execution_completed",
          tool_use_id: call.tool_use_id,
          name: call.name,
          output:
            typeof result.content === "string"
              ? result.content
              : JSON.stringify(result.content),
          is_error: result.is_error,
          is_terminal: result.is_terminal,
          tool_start_time: result.tool_start_time,
          tool_end_time: result.tool_end_time,
        });
      }
      return results;
    },
  };
}

/** An executor over no tools at all. */
export function emptyExecutor(): ToolExecutor {
  return scriptedExecutor();
}

/** A handler that submits a structured outcome as a terminal result. */
export function submitHandler(submission: JsonValue): ScriptedToolHandler {
  return () =>
    Promise.resolve({ content: submission, is_terminal: true });
}

/** Read through a call so control-flow narrowing never caches `aborted`. */
function isAborted(signal: AbortSignal): boolean {
  return signal.aborted;
}

function settleOrAbort(
  work: Promise<ScriptedToolResult>,
  signal: AbortSignal,
): Promise<ScriptedToolResult> {
  return new Promise((resolve) => {
    const onAbort = (): void => {
      resolve({ content: "interrupted", is_error: true });
    };
    signal.addEventListener("abort", onAbort, { once: true });
    work.then(
      (value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort);
        resolve({
          content: error instanceof Error ? error.message : String(error),
          is_error: true,
        });
      },
    );
  });
}

// --- background session handles ---------------------------------------------------

export interface TestSessionHandle {
  handle: {
    settled: Promise<SessionOutcome>;
    cancel(reason: string): Promise<void>;
    describe?(): string;
  };
  settle(outcome: SessionOutcome): void;
  fail(error: Error): void;
  /** Reasons passed to `cancel`, in call order. */
  cancelled: string[];
}

/** A push-settled capability handle; `cancel` resolves per `cancelMode`. */
export function sessionHandle(
  options: { describe?: string; cancelMode?: "resolve" | "hang" } = {},
): TestSessionHandle {
  let settle!: (outcome: SessionOutcome) => void;
  let fail!: (error: Error) => void;
  const settled = new Promise<SessionOutcome>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });
  const cancelled: string[] = [];
  const description = options.describe;
  return {
    handle: {
      settled,
      cancel: (reason) => {
        cancelled.push(reason);
        return options.cancelMode === "hang"
          ? new Promise<void>(() => undefined)
          : Promise.resolve();
      },
      ...(description !== undefined && { describe: () => description }),
    },
    settle,
    fail,
    cancelled,
  };
}

// --- async coordination -------------------------------------------------------

export interface Deferred<T = void> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

export function deferred<T = void>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Rejects when the signal aborts; classification stays on `signal.aborted`. */
function rejectOnAbort(signal: AbortSignal | undefined): Promise<never> {
  return new Promise((_resolve, reject) => {
    if (!signal) return;
    const fail = (): void => {
      reject(new Error("aborted"));
    };
    if (signal.aborted) fail();
    else signal.addEventListener("abort", fail, { once: true });
  });
}

/** One macrotask: every already-queued microtask has run by then. */
export function tick(): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(() => {
      resolve();
    }, 0);
  });
}

// --- loop observer recorder -----------------------------------------------------

export type ObserverCall =
  | { kind: "turnCompleted"; facts: TurnFacts }
  | { kind: "idleStarted" }
  | { kind: "idleEnded" };

/**
 * A recording `LoopObserver`. `onTurnCompleted` lets a test gate the loop
 * (return an unresolved promise) or publish a notification mid-call.
 */
export function recordingObserver(
  onTurnCompleted?: (facts: TurnFacts) => Promise<void> | void,
): { observer: LoopObserver; calls: ObserverCall[] } {
  const calls: ObserverCall[] = [];
  return {
    calls,
    observer: {
      async turnCompleted(facts) {
        calls.push({ kind: "turnCompleted", facts });
        await onTurnCompleted?.(facts);
      },
      idleStarted() {
        calls.push({ kind: "idleStarted" });
      },
      idleEnded() {
        calls.push({ kind: "idleEnded" });
      },
    },
  };
}

// --- run orchestration ---------------------------------------------------------

export interface StartMockRunOptions {
  tools?: ToolExecutor;
  maxTurns?: number;
  signal?: AbortSignal;
  notifications?: NotificationInbox;
  background?: BackgroundSupervisor;
  observer?: LoopObserver;
}

/** Start a run over scripted turns with small defaults. */
export function startMockRun(
  turns: ScriptedTurn[],
  options: StartMockRunOptions = {},
): { client: MockLlmClient; handle: AgentRunHandle } {
  const client = new MockLlmClient(turns);
  const handle = startAgentRun({
    llmClient: client,
    tools: options.tools ?? emptyExecutor(),
    model: "mock-model",
    initialMessages: [userText("hi")],
    maxTokens: 1024,
    maxTurns: options.maxTurns,
    signal: options.signal,
    notifications: options.notifications,
    background: options.background,
    observer: options.observer,
  });
  return { client, handle };
}

/** Drain `handle.events` in the background without ever breaking early. */
export function collectEvents(handle: AgentRunHandle): {
  events: AgentEvent[];
  done: Promise<void>;
} {
  const events: AgentEvent[] = [];
  const done = (async () => {
    for await (const event of handle.events) {
      events.push(event);
    }
  })();
  return { events, done };
}

// --- assertions -----------------------------------------------------------------

export function must<T>(value: T | undefined | null): T {
  if (value === undefined || value === null) {
    throw new Error("expected a value to be present");
  }
  return value;
}

export function asCompleted(
  outcome: AgentRunOutcome,
): Extract<AgentRunOutcome, { status: "completed" }> {
  if (outcome.status !== "completed") {
    throw new Error(`expected a completed outcome, got ${outcome.status}`);
  }
  return outcome;
}

export function asCancelled(
  outcome: AgentRunOutcome,
): Extract<AgentRunOutcome, { status: "cancelled" }> {
  if (outcome.status !== "cancelled") {
    throw new Error(`expected a cancelled outcome, got ${outcome.status}`);
  }
  return outcome;
}

export function asFailed(
  outcome: AgentRunOutcome,
): Extract<AgentRunOutcome, { status: "failed" }> {
  if (outcome.status !== "failed") {
    throw new Error(`expected a failed outcome, got ${outcome.status}`);
  }
  return outcome;
}

/**
 * §6/§7 invariant: the list is valid `LlmRequest.messages` input — roles
 * sane and every `tool_use` answered by a later `tool_result`.
 */
export function expectProviderValid(messages: readonly Message[]): void {
  const unanswered = new Set<string>();
  for (const message of messages) {
    for (const block of message.content) {
      if (block.type === "tool_use") {
        expect(message.role).toBe("assistant");
        unanswered.add(block.tool_use_id);
      } else if (block.type === "tool_result") {
        expect(message.role).toBe("user");
        expect(unanswered.has(block.tool_use_id)).toBe(true);
        unanswered.delete(block.tool_use_id);
      }
    }
  }
  expect([...unanswered]).toEqual([]);
}
