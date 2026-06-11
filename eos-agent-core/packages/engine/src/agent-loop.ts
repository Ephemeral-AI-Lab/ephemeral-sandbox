import { toolUses, type ToolCallResult } from "@eos/contracts";
import { ProviderError, type UsageSnapshot } from "@eos/llm-client";

import type { BackgroundSupervisor } from "./background/supervisor.js";
import type { Conversation, ToolResultBlock } from "./conversation.js";
import type { LoopObserver } from "./loop-observer.js";
import {
  systemNotificationMessage,
  type NotificationInbox,
} from "./notification-inbox.js";
import type { AgentRunFailure, AgentRunStatus, RunHandle } from "./run-handle.js";
import type { ToolExecutor, ToolUseBlock } from "./tool-executor.js";
import { addUsage, runAssistantTurn, type TurnConfig } from "./turn.js";

/**
 * The session-cancel reason the loop's exit path passes to
 * `supervisor.dispose` on every finish. Spawn-site session handles key the
 * disposal cascade off this exact value (Phase 04.5 §8).
 */
export const RUN_FINISHED_DISPOSE_REASON = "run finished";

/** Everything one run's loop needs; assembled by `startAgentRun`. */
export interface AgentLoopContext {
  handle: RunHandle;
  conversation: Conversation;
  tools: ToolExecutor;
  turnConfig: TurnConfig;
  maxTurns: number;
  /** Drained at the loop boundary, one priority below steers. */
  notifications?: NotificationInbox;
  /** Auto-wait gate and dispose-on-finish; sessions are loop lifecycle. */
  background?: BackgroundSupervisor;
  /** Loop-lifecycle announcements (Phase 04.9); never throws or rejects. */
  observer?: LoopObserver;
}

/**
 * The loop spine — control flow only; streaming, transcript writes, and
 * tool dispatch live in their own modules. Never throws: every exit
 * classifies into exactly one `finish`, committed in the same synchronous
 * block as its decision so a late steer is never accepted-but-dropped.
 *
 * Run completion is exclusively a terminal tool result; bare text never
 * terminates (`maxTurns` backstops spin), and the engine appends no
 * reminder on a text turn — that nudge is a notification trigger rule
 * behind the `LoopObserver` port (Phase 04.9).
 */
export async function runAgentLoop(ctx: AgentLoopContext): Promise<void> {
  const { handle, conversation } = ctx;
  let turns = 0;
  let usage: UsageSnapshot = { input_tokens: 0, output_tokens: 0 };
  const finish = (status: AgentRunStatus): void => {
    handle.finish({
      displayed: [...conversation.displayedMessages()],
      llm: [...conversation.llmMessages()],
      usage,
      turns,
      ...status,
    });
  };
  try {
    for (;;) {
      if (isAborted(handle.signal)) {
        finish({ status: "cancelled", reason: handle.cancelReason });
        return;
      }
      if (turns >= ctx.maxTurns) {
        const message = `run spent its ${String(ctx.maxTurns)}-turn budget without completing`;
        finish({ status: "failed", failure: { kind: "max_turns", message } });
        return;
      }
      // Steers first: user input outranks system notices.
      for (const steered of handle.drainSteers()) conversation.appendUser(steered);
      for (const note of ctx.notifications?.drain() ?? []) {
        conversation.appendUser(note);
      }
      handle.emit({ type: "turn_started", turn: turns + 1 });
      const turn = await runAssistantTurn(ctx.turnConfig, conversation, handle.signal, handle.emit);
      conversation.appendAssistant(turn.message);
      turns += 1;
      usage = addUsage(usage, turn.usage);
      const calls = toolUses(turn.message);
      await ctx.observer?.turnCompleted({
        turn: turns,
        maxTurns: ctx.maxTurns,
        toolCalls: calls.length,
        liveSessions: ctx.background?.liveCount() ?? 0,
        hasPendingSteers: handle.hasPendingSteers(),
      });
      if (calls.length === 0) {
        // Auto-wait: a no-tool-use turn with live sessions parks on the
        // next notification OR steer instead of burning provider calls.
        // Waiting consumes no turn; an abort wakes the race and the
        // loop-top check classifies it.
        if (!handle.hasPendingSteers() && (ctx.background?.liveCount() ?? 0) > 0) {
          ctx.observer?.idleStarted();
          try {
            await waitForWake(ctx);
          } finally {
            ctx.observer?.idleEnded();
          }
        }
        continue;
      }
      const results = normalizeBatch(
        calls,
        await ctx.tools.executeBatch(calls, handle.signal, handle.emit),
      );
      conversation.appendToolResults(results.map(projectToolResult));
      publishHookContexts(results, ctx.notifications);
      const terminal = results.find((result) => result.is_terminal);
      if (terminal) {
        // The submission outranks late redirection: steers accepted
        // mid-batch die with the run.
        finish({
          status: "completed",
          final_message: turn.message,
          stop_reason: turn.stop_reason,
          submission: terminal.content,
        });
        return;
      }
      if (isAborted(handle.signal)) {
        finish({ status: "cancelled", reason: handle.cancelReason });
        return;
      }
    }
  } catch (error) {
    finish(classifyLoopError(error, handle));
  } finally {
    if (!handle.finished) {
      const failure: AgentRunFailure = { kind: "internal", message: "agent loop exited without finishing" };
      finish({ status: "failed", failure });
    }
    // Sessions are loop lifecycle: every finish tears them down.
    // Fire-and-forget — run_finished never waits on teardown.
    void ctx.background?.dispose(RUN_FINISHED_DISPOSE_REASON);
  }
}

/**
 * Race the inbox against the steer queue; both waits resolve on abort.
 * The race loser would otherwise stay registered (waker + abort listener)
 * until an unrelated wake event, so a race-scoped signal unhooks it as
 * soon as the winner settles.
 */
async function waitForWake(ctx: AgentLoopContext): Promise<void> {
  const settled = new AbortController();
  const signal = AbortSignal.any([ctx.handle.signal, settled.signal]);
  const waits = [ctx.handle.waitForSteer(signal)];
  if (ctx.notifications) waits.push(ctx.notifications.waitForNext(signal));
  try {
    await Promise.race(waits);
  } finally {
    settled.abort();
  }
}

/**
 * The engine-owned invariant (Phase 03 §7): every `tool_use_id` is
 * answered, in `tool_use` order, regardless of executor behavior. A
 * missing result becomes a synthetic error; no event is emitted for it.
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

/**
 * The one publisher of hook `additionalContext` (Phase 04.5 decision 11):
 * each `metadata.hook_contexts` entry becomes a `hook_context` notification
 * as the result is appended, drained at the next loop boundary.
 */
function publishHookContexts(
  results: ToolCallResult[],
  notifications: NotificationInbox | undefined,
): void {
  if (!notifications) return;
  for (const result of results) {
    const contexts = result.metadata?.hook_contexts;
    if (!Array.isArray(contexts)) continue;
    for (const text of contexts) {
      if (typeof text !== "string") continue;
      notifications.publish(
        systemNotificationMessage({
          type: "hook_context",
          tool_use_id: result.tool_use_id,
          text,
        }),
      );
    }
  }
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

function classifyLoopError(error: unknown, handle: RunHandle): AgentRunStatus {
  if (isAborted(handle.signal)) {
    return { status: "cancelled", reason: handle.cancelReason };
  }
  const failure: AgentRunFailure =
    error instanceof ProviderError
      ? { kind: "provider_error", message: error.message }
      : { kind: "internal", message: error instanceof Error ? error.message : String(error) };
  return { status: "failed", failure };
}

/** Read through a call so control-flow narrowing never caches `aborted`. */
function isAborted(signal: AbortSignal): boolean {
  return signal.aborted;
}
