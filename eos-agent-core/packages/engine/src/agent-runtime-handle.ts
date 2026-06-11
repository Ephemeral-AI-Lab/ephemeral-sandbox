import type { JsonObject, JsonValue, Message, ToolUseId } from "@eos/contracts";
import type { LlmStreamEvent, StopReason, UsageSnapshot } from "@eos/llm-client";

/** Why a salvaged partial assistant message never completed. */
export type PartialReason = "interrupted" | "provider_error";

/** One entry of the user-facing transcript. */
export interface DisplayedMessage {
  /** Monotonic within the run. */
  seq: number;
  /** ISO-8601 append time. */
  created_at: string;
  message: Message;
  /** Set only on salvaged partial assistant output (displayed-only). */
  partial?: PartialReason;
}

/**
 * The run event union: provider stream events forwarded unchanged, plus tool
 * execution and run lifecycle. Payload fields are snake_case because this
 * union crosses runtime, transcript, and future transport boundaries.
 */
export type AgentEvent =
  | {
      /** A provider call is about to start; 1-based, after the steer drain. */
      type: "turn_started";
      turn: number;
    }
  | LlmStreamEvent
  | {
      /** A tool call left the queue and began executing. */
      type: "tool_execution_started";
      tool_use_id: ToolUseId;
      name: string;
      input: JsonObject;
    }
  | {
      /** A tool call settled (result, mapped error, or unknown tool). */
      type: "tool_execution_completed";
      tool_use_id: ToolUseId;
      name: string;
      /** String projection of the result content. */
      output: string;
      is_error: boolean;
      is_terminal: boolean;
      /** Epoch ms; brackets `execute()` only, never hook time. */
      tool_start_time: number;
      tool_end_time: number;
      metadata?: JsonObject;
    }
  | {
      /** Terminal event; the iterable completes after it. */
      type: "run_finished";
      outcome: AgentRunOutcome;
    };

/**
 * A push-fed queue consumed as one pull-based `AsyncIterable`:
 *
 * - single consumer: a second `[Symbol.asyncIterator]()` call throws,
 * - pushes never block the loop; the buffer is unbounded (the consumer is
 *   in-process; backpressure belongs to the server phase),
 * - `close()` completes iteration once the buffer drains,
 * - an early `break`/`return()` detaches: later pushes are discarded while
 *   the run continues; a stream nobody iterates retains every event.
 */
export class EventStream implements AsyncIterable<AgentEvent> {
  #buffer: AgentEvent[] = [];
  #wakers: (() => void)[] = [];
  #closed = false;
  #detached = false;
  #consumed = false;

  push(event: AgentEvent): void {
    if (this.#detached) return;
    this.#buffer.push(event);
    this.#wake();
  }

  close(): void {
    this.#closed = true;
    this.#wake();
  }

  [Symbol.asyncIterator](): AsyncIterator<AgentEvent, undefined> {
    if (this.#consumed) {
      throw new Error("EventStream supports a single consumer");
    }
    this.#consumed = true;
    return {
      next: () => this.#next(),
      return: () => {
        this.#detach();
        return Promise.resolve<IteratorResult<AgentEvent, undefined>>({
          done: true,
          value: undefined,
        });
      },
    };
  }

  async #next(): Promise<IteratorResult<AgentEvent, undefined>> {
    for (;;) {
      if (this.#detached) return { done: true, value: undefined };
      const event = this.#buffer.shift();
      if (event) return { done: false, value: event };
      if (this.#closed) return { done: true, value: undefined };
      await new Promise<void>((resolve) => {
        this.#wakers.push(resolve);
      });
    }
  }

  #detach(): void {
    this.#detached = true;
    this.#buffer.length = 0;
    this.#wake();
  }

  #wake(): void {
    const wakers = this.#wakers;
    this.#wakers = [];
    for (const waker of wakers) waker();
  }
}

/** Why a run failed, typed so callers never parse prose. */
export interface AgentRunFailure {
  /** `max_turns` is restartable with a fresh budget. */
  kind: "provider_error" | "max_turns" | "internal";
  message: string;
}

/** The status arm of `AgentRunOutcome`; assembled at each loop exit. */
export type AgentRunStatus =
  | {
      status: "completed";
      /** The assistant message that carried the terminal tool call. */
      final_message: Message;
      stop_reason?: StopReason;
      /** The terminal tool result's structured content. */
      submission?: JsonValue;
    }
  | { status: "cancelled"; reason: string }
  | { status: "failed"; failure: AgentRunFailure };

/**
 * The terminal state of one run. `llm` is provider-valid restart input on
 * every status: each `tool_use` is answered (synthetic results on cancel)
 * and salvaged partials never land in it.
 */
export type AgentRunOutcome = {
  displayed: DisplayedMessage[];
  llm: Message[];
  /** Summed across completed turns. */
  usage: UsageSnapshot;
  turns: number;
} & AgentRunStatus;

/** The public surface of one running agent loop. */
export interface AgentRunHandle {
  /** Single-consumer event stream; `run_finished` is always last. */
  events: AsyncIterable<AgentEvent>;
  /** Resolves after `run_finished` is enqueued; never rejects. */
  outcome: Promise<AgentRunOutcome>;
  /**
   * Queue a user message for the next turn boundary; throws `TypeError` on
   * a non-user role and returns false once finishing has begun.
   */
  steer(message: Message): boolean;
  /**
   * The one stop semantic: abort the run's signal. Idempotent, no-op after
   * finish. `reason` is a label recorded on the cancelled outcome, never a
   * behavior branch.
   */
  interrupt(reason?: string): void;
}

/**
 * Run-handle internals shared with the loop: one event stream, one abort
 * signal (optionally child of a caller scope), one steer queue drained at
 * turn boundaries, and one atomic finish.
 */
export class RunHandle implements AgentRunHandle {
  readonly signal: AbortSignal;
  readonly outcome: Promise<AgentRunOutcome>;
  readonly #stream = new EventStream();
  readonly #controller = new AbortController();
  #steers: Message[] = [];
  readonly #steerWakers = new Set<() => void>();
  #finished = false;
  #cancelReason: string | undefined;
  #resolveOutcome!: (outcome: AgentRunOutcome) => void;

  constructor(parent?: AbortSignal) {
    this.signal = parent
      ? AbortSignal.any([this.#controller.signal, parent])
      : this.#controller.signal;
    this.outcome = new Promise((resolve) => {
      this.#resolveOutcome = resolve;
    });
  }

  get events(): AsyncIterable<AgentEvent> {
    return this.#stream;
  }

  get finished(): boolean {
    return this.#finished;
  }

  /** The recorded interrupt label; external aborts record none. */
  get cancelReason(): string {
    return this.#cancelReason ?? "interrupted";
  }

  steer(message: Message): boolean {
    if (message.role !== "user") {
      throw new TypeError("steer() requires a user message");
    }
    if (this.#finished) return false;
    this.#steers.push(message);
    for (const wake of [...this.#steerWakers]) wake();
    return true;
  }

  interrupt(reason?: string): void {
    if (this.#finished) return;
    this.#cancelReason ??= reason ?? "interrupted";
    this.#controller.abort();
  }

  /** Loop step 3: take every queued steer, in arrival order. */
  drainSteers(): Message[] {
    const drained = this.#steers;
    this.#steers = [];
    return drained;
  }

  hasPendingSteers(): boolean {
    return this.#steers.length > 0;
  }

  /**
   * Level-triggered wait backing the loop's auto-wait race: resolves
   * immediately if steers are pending, on the next arrival, or on abort -
   * so a user can redirect or interrupt a run parked on slow sessions.
   */
  waitForSteer(signal: AbortSignal): Promise<void> {
    if (this.#steers.length > 0 || signal.aborted) return Promise.resolve();
    return new Promise((resolve) => {
      const wake = (): void => {
        this.#steerWakers.delete(wake);
        signal.removeEventListener("abort", wake);
        resolve();
      };
      this.#steerWakers.add(wake);
      signal.addEventListener("abort", wake);
    });
  }

  readonly emit = (event: AgentEvent): void => {
    this.#stream.push(event);
  };

  /**
   * The atomic finish: flips `steer()` to false, emits `run_finished`,
   * closes the stream, and resolves `outcome` — exactly once, in one
   * synchronous block.
   */
  finish(outcome: AgentRunOutcome): void {
    if (this.#finished) return;
    this.#finished = true;
    this.#steers = [];
    this.#stream.push({ type: "run_finished", outcome });
    this.#stream.close();
    this.#resolveOutcome(outcome);
  }
}
