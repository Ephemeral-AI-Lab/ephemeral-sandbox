import type { AgentRunId, JsonObject, ToolUseId, UserMessage } from "../contracts/index.js";
import type {
  BackgroundTaskLifecycleEvent,
  BackgroundTaskSupervisor,
} from "../background/index.js";
import type { LlmStreamEvent, UsageSnapshot } from "../llm-client/index.js";
import type { Notifier } from "../notification/index.js";

/**
 * The run event union before the handle stamps `seq`: run lifecycle,
 * tool execution, background-task lifecycle, and the provider stream
 * events forwarded unchanged. Older members keep their snake_case payload
 * fields; the task events mirror the public camelCase task vocabulary.
 */
export type AgentEventBody =
  | { type: "run_started"; run_id: AgentRunId; agent_name: string }
  | {
      /** A provider call is about to start; 1-based, after the drains. */
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
  | BackgroundTaskLifecycleEvent
  | {
      /** Terminal event; the live iterable completes after it. */
      type: "run_finished";
      outcome: AgentOutcome<unknown>;
    };

/** Every event carries `seq`; records serve resume, the stream is live-only. */
export type AgentEvent = AgentEventBody & { seq: number };

/** Why a run failed, typed so callers never parse prose. */
export interface AgentRunError {
  /** `max_turns` is restartable with a fresh budget. */
  kind: "max_turns" | "provider_error" | "internal";
  message: string;
}

/**
 * The terminal state of one run: the terminal tool's accepted submission
 * (or the final text in text mode), a failure, or a cancellation — always
 * with the run's summed usage and turn count.
 */
export type AgentOutcome<T = string> = {
  /** Summed across completed turns. */
  usage: UsageSnapshot;
  turns: number;
} & (
  | { status: "completed"; outcome: T }
  | { status: "failed"; error: AgentRunError }
  | { status: "cancelled" }
);

/**
 * The public surface of one live run — the only capability for it; hosts
 * that need by-id access keep their own map.
 */
export interface AgentRunHandle<T = string> {
  runId: AgentRunId;
  /** Queue a user message for the next boundary; false once finishing has begun. */
  steer(message: UserMessage): boolean;
  /** The one stop semantic. Idempotent, no-op after finish. */
  interrupt(): void;
  /**
   * Totality: always resolves, never rejects; memoized — callable any
   * number of times, before or after the run finishes.
   */
  outcome(): Promise<AgentOutcome<T>>;
  /** Live-only, single consumer; `run_finished` is always last. */
  events(): AsyncIterable<AgentEvent>;
  backgroundTaskSupervisor: BackgroundTaskSupervisor;
  notifier: Notifier;
}

/**
 * A push-fed queue consumed as one pull-based `AsyncIterable`:
 *
 * - single consumer: a second `[Symbol.asyncIterator]()` call throws,
 * - pushes never block the loop; the buffer is unbounded (the consumer is
 *   in-process; fan-out and backpressure are host concerns),
 * - `close()` completes iteration once the buffer drains; later pushes
 *   are dropped (run-end disposal settles tasks after `run_finished`),
 * - an early `break`/`return()` detaches: later pushes are discarded while
 *   the run continues; a stream nobody iterates retains every event.
 */
class EventStream implements AsyncIterable<AgentEvent> {
  #buffer: AgentEvent[] = [];
  #wakers: (() => void)[] = [];
  #closed = false;
  #detached = false;
  #consumed = false;

  push(event: AgentEvent): void {
    if (this.#detached || this.#closed) return;
    this.#buffer.push(event);
    this.#wake();
  }

  close(): void {
    this.#closed = true;
    this.#wake();
  }

  [Symbol.asyncIterator](): AsyncIterator<AgentEvent, undefined> {
    if (this.#consumed) {
      throw new Error("events() supports a single consumer");
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

export interface RunHandleDeps {
  runId: AgentRunId;
  backgroundTaskSupervisor: BackgroundTaskSupervisor;
  notifier: Notifier;
  /** Records sink: sees every stamped event, including post-finish task settles. */
  tap?: (event: AgentEvent) => void;
}

/**
 * Run-handle internals shared with the loop: one seq-stamping emitter, one
 * event stream, one abort signal, one steer queue drained at boundaries,
 * a finishing latch (flipped while a submission commits), and one atomic
 * finish.
 */
export class RunHandle<T = string> implements AgentRunHandle<T> {
  readonly runId: AgentRunId;
  readonly backgroundTaskSupervisor: BackgroundTaskSupervisor;
  readonly notifier: Notifier;
  readonly signal: AbortSignal;
  readonly #tap: ((event: AgentEvent) => void) | undefined;
  readonly #stream = new EventStream();
  readonly #controller = new AbortController();
  #seq = 0;
  #steers: UserMessage[] = [];
  readonly #steerWakers = new Set<() => void>();
  #finishing = false;
  #finished = false;
  #resolveOutcome!: (outcome: AgentOutcome<T>) => void;
  readonly #outcome: Promise<AgentOutcome<T>>;

  constructor(deps: RunHandleDeps) {
    this.runId = deps.runId;
    this.backgroundTaskSupervisor = deps.backgroundTaskSupervisor;
    this.notifier = deps.notifier;
    this.#tap = deps.tap;
    this.signal = this.#controller.signal;
    this.#outcome = new Promise((resolve) => {
      this.#resolveOutcome = resolve;
    });
  }

  events(): AsyncIterable<AgentEvent> {
    return this.#stream;
  }

  outcome(): Promise<AgentOutcome<T>> {
    return this.#outcome;
  }

  get finished(): boolean {
    return this.#finished;
  }

  steer(message: UserMessage): boolean {
    if ((message as { role: string }).role !== "user") {
      throw new TypeError("steer() requires a user message");
    }
    if (this.#finishing || this.#finished) return false;
    this.#steers.push(message);
    for (const wake of [...this.#steerWakers]) wake();
    return true;
  }

  interrupt(): void {
    if (this.#finished) return;
    this.#controller.abort();
  }

  /**
   * The seq-stamping emitter: every event flows through here exactly once
   * — records tap first (lossless even with no live consumer), then the
   * live stream.
   */
  readonly emit = (body: AgentEventBody): void => {
    const event: AgentEvent = { ...body, seq: this.#seq };
    this.#seq += 1;
    this.#tap?.(event);
    this.#stream.push(event);
  };

  /** Loop boundary: take every queued steer, in arrival order. */
  drainSteers(): UserMessage[] {
    const drained = this.#steers;
    this.#steers = [];
    return drained;
  }

  hasPendingSteers(): boolean {
    return this.#steers.length > 0;
  }

  /**
   * Level-triggered wait backing the park: resolves immediately if steers
   * are pending, on the next arrival, or on abort.
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

  /**
   * The finishing latch around `onSubmit`: between the gate passing and
   * the handler's verdict, `steer()` returns false so a commit-window
   * steer is refused instead of accepted-but-dropped. A rejection reopens
   * the run.
   */
  beginFinishing(): void {
    this.#finishing = true;
  }

  cancelFinishing(): void {
    if (!this.#finished) this.#finishing = false;
  }

  /**
   * The atomic finish: flips `steer()` to false, emits `run_finished`,
   * closes the stream, and resolves `outcome()` — exactly once, in one
   * synchronous block.
   */
  finish(outcome: AgentOutcome<T>): void {
    if (this.#finished) return;
    this.#finishing = true;
    this.#finished = true;
    this.#steers = [];
    this.emit({ type: "run_finished", outcome: outcome as AgentOutcome<unknown> });
    this.#stream.close();
    this.#resolveOutcome(outcome);
  }
}
