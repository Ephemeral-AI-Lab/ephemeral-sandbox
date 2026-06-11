import type {
  AgentRunSnapshot,
  ToolCallResult,
  ToolSpec,
  ToolUseId,
} from "@eos/contracts";
import type { AgentEvent, ToolExecutor, ToolUseBlock } from "@eos/engine";

import { projectContent, type BoundTool } from "./pipeline.js";
import { snapshotRunState, type AgentRunState } from "./run-state.js";

const MAX_TOOL_CONCURRENCY = 8;

export interface ToolBatchExecutorInput {
  runState: AgentRunState;
  /** Already bound and deterministically ordered by the assembler. */
  tools: BoundTool[];
}

/**
 * The `ToolExecutor` the engine injects. Per-turn `specs()` filters by
 * workspace mode; `executeBatch` keeps the Phase 03 runner semantics -
 * fully concurrent under a cap of 8, results in `tool_use` order, thrown
 * errors and unknown names mapped to `is_error` results, abort settling
 * with straggler-emit suppression - and adds the batch-execution-forbidden
 * policy: a batch-forbidden call (e.g. a terminal submission) with any
 * sibling rejects the WHOLE batch undispatched.
 */
export function toolBatchExecutor(input: ToolBatchExecutorInput): ToolExecutor {
  const { runState, tools } = input;
  const byName = new Map(tools.map((tool) => [tool.definition.name as string, tool]));
  return {
    specs(): ToolSpec[] {
      return tools
        .filter(
          (tool) =>
            !runState.workspace.isIsolated ||
            tool.definition.availableInIsolatedWorkspace,
        )
        .map((tool) => tool.definition.spec);
    },
    async executeBatch(
      calls: ToolUseBlock[],
      signal: AbortSignal,
      emit: (event: AgentEvent) => void,
    ): Promise<ToolCallResult[]> {
      // One snapshot per batch: every sibling's meta is built from it, so
      // a mid-batch workspace flip applies at the next turn boundary.
      const run = snapshotRunState(runState);
      const rejection = forbiddenBatchRejection(calls, byName);
      if (rejection !== undefined) {
        return calls.map((call) => errorResult(call.tool_use_id, rejection));
      }
      const settled = new Array<ToolCallResult | undefined>(calls.length);
      let cursor = 0;
      const worker = async (): Promise<void> => {
        while (cursor < calls.length && !signal.aborted) {
          const index = cursor;
          cursor += 1;
          const call = calls[index];
          emit({
            type: "tool_execution_started",
            tool_use_id: call.tool_use_id,
            name: call.name,
            input: call.input,
          });
          const result = await executeCall(call, byName, run, signal);
          // The batch already settled with a synthetic result; drop the
          // straggler so no event lands after run_finished.
          if (isAborted(signal)) return;
          settled[index] = result;
          emit({
            type: "tool_execution_completed",
            tool_use_id: call.tool_use_id,
            name: call.name,
            output: projectContent(result.content),
            is_error: result.is_error,
            is_terminal: result.is_terminal,
            tool_start_time: result.tool_start_time,
            tool_end_time: result.tool_end_time,
            ...(result.metadata !== undefined && { metadata: result.metadata }),
          });
        }
      };
      const workers = Promise.all(
        Array.from({ length: Math.min(MAX_TOOL_CONCURRENCY, calls.length) }, () =>
          worker(),
        ),
      );
      await settledOrAborted(workers, signal);
      return calls.map(
        (call, index) => settled[index] ?? errorResult(call.tool_use_id, "interrupted"),
      );
    },
  };
}

/**
 * Batch-execution-forbidden policy: a batch with a flagged call plus any
 * sibling rejects every call; a solo flagged call dispatches normally.
 */
function forbiddenBatchRejection(
  calls: ToolUseBlock[],
  byName: Map<string, BoundTool>,
): string | undefined {
  if (calls.length <= 1) return undefined;
  const flagged = [
    ...new Set(
      calls
        .filter((call) => byName.get(call.name)?.definition.isBatchExecutionForbidden)
        .map((call) => `\`${call.name}\``),
    ),
  ].sort();
  if (flagged.length === 0) return undefined;
  return `tool ${flagged.join(", ")} must be called alone; the whole batch was rejected without dispatching`;
}

async function executeCall(
  call: ToolUseBlock,
  byName: Map<string, BoundTool>,
  run: AgentRunSnapshot,
  signal: AbortSignal,
): Promise<ToolCallResult> {
  const tool = byName.get(call.name);
  if (!tool) return errorResult(call.tool_use_id, `tool not found: ${call.name}`);
  try {
    return { tool_use_id: call.tool_use_id, ...(await tool.run(call, run, signal)) };
  } catch (error) {
    // The pipeline never throws by contract; this keeps a faulting call
    // from cascading into siblings all the same.
    return errorResult(
      call.tool_use_id,
      error instanceof Error ? error.message : String(error),
    );
  }
}

/** A settled error result with both clocks at the settling instant. */
function errorResult(toolUseId: ToolUseId, content: string): ToolCallResult {
  const at = Date.now();
  return {
    tool_use_id: toolUseId,
    content,
    is_error: true,
    is_terminal: false,
    tool_start_time: at,
    tool_end_time: at,
  };
}

/** Read through a call so control-flow narrowing never caches `aborted`. */
function isAborted(signal: AbortSignal): boolean {
  return signal.aborted;
}

/** Resolve when every worker settles or the signal aborts, whichever first. */
async function settledOrAborted(
  workers: Promise<unknown>,
  signal: AbortSignal,
): Promise<void> {
  if (signal.aborted) return;
  let onAbort: (() => void) | undefined;
  const aborted = new Promise<void>((resolve) => {
    onAbort = () => {
      resolve();
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
  try {
    await Promise.race([workers, aborted]);
  } finally {
    if (onAbort) signal.removeEventListener("abort", onAbort);
  }
}
