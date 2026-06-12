import {
  assistantText,
  toolUses,
  type Message,
  type ToolCallResult,
} from "../contracts/index.js";
import { ProviderError, type UsageSnapshot } from "../llm-client/index.js";
import {
  systemNotificationMessage,
  type NotificationInbox,
} from "../notification/index.js";

import type { Conversation, ToolResultBlock } from "./conversation.js";
import type { AgentRunError, RunHandle } from "./run-handle.js";
import type { ToolExecutor, ToolUseBlock } from "./tool-executor.js";
import { addUsage, runAssistantTurn, type TurnConfig } from "./turn.js";

/** The cancel reason run-end disposal passes to the task registry. */
const RUN_FINISHED_DISPOSE_REASON = "run finished";

/**
 * Loop facts handed to `turnBoundary` hooks after each committed assistant
 * turn; in-process camelCase. Two axes: the SHAPE of the turn that just
 * committed (`toolCalls`, `backgroundTaskCount`, `hasPendingSteers`) and
 * the run's BUDGET position (`turn`, `maxTurns`).
 */
export interface TurnFacts {
  /** Budget axis: 1-based number of the turn that just committed. */
  turn: number;
  /** Budget axis: the run's fixed turn budget. */
  maxTurns: number;
  /** Shape axis: `tool_use` blocks in this turn; 0 means bare text. */
  toolCalls: number;
  /** Shape axis: registry size (running + settling) at this boundary. */
  backgroundTaskCount: number;
  /** Shape axis: a user steer is already queued at this boundary. */
  hasPendingSteers: boolean;
}

/**
 * The loop's view of the run's task registry — the same registry the
 * public supervisor mutates; the gates are SDK-internal mechanism, so
 * these members are deliberately absent from the public interface.
 */
export interface TaskRegistryGate {
  isEmpty(): boolean;
  count(): number;
  changeCount(): number;
  waitForChange(since: number, signal: AbortSignal): Promise<void>;
  disposeAll(reason: string): Promise<void>;
}

/** How the run completes; fixed at assembly from `agentOutcomeFn` presence. */
export type TerminationMode<T> =
  | { kind: "text" }
  | {
      /** Reads the submission the terminal tool accepted in this batch. */
      kind: "terminal";
      takeAccepted: () => { value: T } | undefined;
    };

/** Everything one run's loop needs; assembled by the runtime. */
export interface AgentLoopContext<T> {
  handle: RunHandle<T>;
  conversation: Conversation;
  tools: ToolExecutor;
  turnConfig: TurnConfig;
  maxTurns: number;
  /** Drained at loop boundaries, one priority below steers. */
  inbox: NotificationInbox;
  /** The gates' and park's registry view; disposed on every finish. */
  tasks: TaskRegistryGate;
  /** turnBoundary hook dispatch; never throws (the runtime wraps it). */
  onTurnBoundary?: (facts: TurnFacts) => Promise<void>;
  mode: TerminationMode<T>;
}

/**
 * The loop spine — control flow only; streaming, records, and tool
 * dispatch live in their own modules. Never throws: every exit classifies
 * into exactly one `finish`, committed in the same synchronous block as
 * its decision so a late steer is never accepted-but-dropped.
 *
 * One predicate gates both exits: `calls == 0 ∧ no pending steers ∧ task
 * registry empty ∧ inbox drained`. In text mode a bare-text turn finishes
 * the run when the gate is open; in terminal-tool mode only an accepted
 * submission does (the terminal binding evaluates the same gate at
 * submission time). `calls == 0 ∧ no steers ∧ inbox drained ∧ registry
 * non-empty` parks the run; wake sources are any inbox publish, any task
 * removal, any steer, and interrupt. A wake whose drain yields nothing
 * re-evaluates the gate: text mode completes with the existing final
 * text, terminal mode re-prompts the model (`maxTurns` backstops spin).
 */
export async function runAgentLoop<T>(ctx: AgentLoopContext<T>): Promise<void> {
  const { handle, conversation, inbox, tasks } = ctx;
  let turns = 0;
  let usage: UsageSnapshot = { input_tokens: 0, output_tokens: 0 };
  /** Text mode: the bare-text message awaiting an open gate. */
  let pendingText: Message | undefined;
  /** The last committed turn made no calls — gate/park territory. */
  let atRest = false;
  const finish = (
    status:
      | { status: "completed"; outcome: T }
      | { status: "failed"; error: AgentRunError }
      | { status: "cancelled" },
  ): void => {
    handle.finish({ usage, turns, ...status });
  };
  try {
    for (;;) {
      if (isAborted(handle.signal)) {
        finish({ status: "cancelled" });
        return;
      }
      // Captured before the drains and gate checks: a task removal racing
      // this boundary advances the counter and the park resolves at once.
      const tasksSince = tasks.changeCount();
      // Steers first: user input outranks system notices.
      const steers = handle.drainSteers();
      for (const steered of steers) conversation.appendUser(steered, "steer");
      const notes = inbox.drain();
      for (const note of notes) {
        conversation.appendUser(
          systemNotificationMessage({ message: note }),
          "notification",
        );
      }
      if (atRest && steers.length === 0 && notes.length === 0) {
        if (tasks.isEmpty()) {
          if (ctx.mode.kind === "text" && pendingText !== undefined) {
            // Empty wake with the gate open: complete with the existing
            // final text instead of burning a provider call. Text mode
            // fixes T = string; the cast localizes that contract.
            finish({
              status: "completed",
              outcome: assistantText(pendingText) as unknown as T,
            });
            return;
          }
          // Terminal mode: the empty wake re-prompts the model below.
        } else {
          // Park (auto-wait): nothing to tell the model, work still open.
          await waitForWake(ctx, tasksSince);
          continue;
        }
      }
      if (turns >= ctx.maxTurns) {
        const message = `run spent its ${String(ctx.maxTurns)}-turn budget without completing`;
        finish({ status: "failed", error: { kind: "max_turns", message } });
        return;
      }
      atRest = false;
      pendingText = undefined;
      handle.emit({ type: "turn_started", turn: turns + 1 });
      const turn = await runAssistantTurn(
        ctx.turnConfig,
        conversation,
        handle.signal,
        handle.emit,
      );
      conversation.appendAssistant(turn.message);
      turns += 1;
      usage = addUsage(usage, turn.usage);
      const calls = toolUses(turn.message);
      const facts: TurnFacts = {
        turn: turns,
        maxTurns: ctx.maxTurns,
        toolCalls: calls.length,
        backgroundTaskCount: tasks.count(),
        hasPendingSteers: handle.hasPendingSteers(),
      };
      if (calls.length === 0) {
        if (
          ctx.mode.kind === "text" &&
          !handle.hasPendingSteers() &&
          tasks.isEmpty() &&
          inbox.isEmpty()
        ) {
          // The text exit: the same predicate as the terminal-submission
          // gate, decided and committed synchronously — no boundary hook
          // runs on the finishing turn (a publish into a finished run
          // informs nobody).
          finish({
            status: "completed",
            outcome: assistantText(turn.message) as unknown as T,
          });
          return;
        }
        pendingText = ctx.mode.kind === "text" ? turn.message : undefined;
        atRest = true;
        await ctx.onTurnBoundary?.(facts);
        continue;
      }
      await ctx.onTurnBoundary?.(facts);
      const results = normalizeBatch(
        calls,
        await ctx.tools.executeBatch(calls, {
          signal: handle.signal,
          emit: handle.emit,
          llmMessages: [...conversation.llmMessages()],
        }),
      );
      conversation.appendToolResults(results.map(projectToolResult));
      const terminal = results.find((result) => result.is_terminal);
      if (terminal) {
        const accepted =
          ctx.mode.kind === "terminal" ? ctx.mode.takeAccepted() : undefined;
        if (accepted) {
          finish({ status: "completed", outcome: accepted.value });
        } else {
          finish({
            status: "failed",
            error: {
              kind: "internal",
              message: "terminal result without an accepted submission",
            },
          });
        }
        return;
      }
      if (isAborted(handle.signal)) {
        finish({ status: "cancelled" });
        return;
      }
    }
  } catch (error) {
    finish(classifyLoopError(error, handle.signal));
  } finally {
    if (!handle.finished) {
      finish({
        status: "failed",
        error: { kind: "internal", message: "agent loop exited without finishing" },
      });
    }
    // Run-end disposal: every finish tears the registry down. Fire and
    // forget — run_finished never waits on teardown; the records sink
    // still sees the post-finish task_settled events.
    void ctx.tasks.disposeAll(RUN_FINISHED_DISPOSE_REASON);
  }
}

/**
 * Park until any wake source fires: a steer, an inbox publish, a task
 * removal, or abort. The race loser would otherwise stay registered until
 * an unrelated wake, so a race-scoped signal unhooks it as soon as the
 * winner settles.
 */
async function waitForWake<T>(
  ctx: AgentLoopContext<T>,
  tasksSince: number,
): Promise<void> {
  const settled = new AbortController();
  const signal = AbortSignal.any([ctx.handle.signal, settled.signal]);
  try {
    await Promise.race([
      ctx.handle.waitForSteer(signal),
      ctx.inbox.waitForNext(signal),
      ctx.tasks.waitForChange(tasksSince, signal),
    ]);
  } finally {
    settled.abort();
  }
}

/**
 * The engine-owned invariant: every `tool_use_id` is answered, in
 * `tool_use` order, regardless of executor behavior. A missing result
 * becomes a synthetic error; no event is emitted for it.
 */
function normalizeBatch(
  calls: ToolUseBlock[],
  results: ToolCallResult[],
): ToolCallResult[] {
  const byId = new Map(results.map((result) => [result.tool_use_id, result]));
  return calls.map((call) => {
    const settled = byId.get(call.tool_use_id);
    if (settled) return settled;
    const at = Date.now();
    return {
      tool_use_id: call.tool_use_id,
      content: "interrupted",
      is_error: true,
      is_terminal: false,
      tool_start_time: at,
      tool_end_time: at,
    };
  });
}

/** The ONE serialization point: non-string content is stringified here. */
function projectToolResult(result: ToolCallResult): ToolResultBlock {
  return {
    type: "tool_result",
    tool_use_id: result.tool_use_id,
    content:
      typeof result.content === "string"
        ? result.content
        : JSON.stringify(result.content),
    is_error: result.is_error,
  };
}

function classifyLoopError(
  error: unknown,
  signal: AbortSignal,
): { status: "cancelled" } | { status: "failed"; error: AgentRunError } {
  if (isAborted(signal)) return { status: "cancelled" };
  const failure: AgentRunError =
    error instanceof ProviderError
      ? { kind: "provider_error", message: error.message }
      : {
          kind: "internal",
          message: error instanceof Error ? error.message : String(error),
        };
  return { status: "failed", error: failure };
}

/** Read through a call so control-flow narrowing never caches `aborted`. */
function isAborted(signal: AbortSignal): boolean {
  return signal.aborted;
}
