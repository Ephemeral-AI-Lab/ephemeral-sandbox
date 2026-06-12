import type { ToolCallResult, ToolSpec, ToolUseId } from "../contracts/index.js";
import type {
  ToolBatchContext,
  ToolExecutor,
  ToolUseBlock,
} from "../engine/index.js";

import type { BoundTool, PipelineResult } from "./pipeline.js";

const MAX_TOOL_CONCURRENCY = 8;

export interface ToolBatchExecutorInput {
  /** Already bound and deterministically ordered by the assembler. */
  tools: BoundTool[];
  /** The terminal tool's name, when the run has one. */
  terminalName?: string;
}

/**
 * The `ToolExecutor` the engine injects. Ordinary calls run fully
 * concurrent under a cap of 8, results in `tool_use` order, thrown errors
 * and unknown names mapped to `is_error` results, abort settling with
 * straggler-emit suppression. Terminal calls execute AFTER every sibling
 * call has resolved (§4.1: the gate is evaluated at that point), in call
 * order; once one is accepted the rest of the batch's terminal calls are
 * denied by the gate's finishing latch.
 */
export function toolBatchExecutor(input: ToolBatchExecutorInput): ToolExecutor {
  const byName = new Map(input.tools.map((tool) => [tool.name, tool]));
  return {
    specs(): ToolSpec[] {
      return input.tools.map((tool) => tool.spec);
    },
    async executeBatch(
      calls: ToolUseBlock[],
      batch: ToolBatchContext,
    ): Promise<ToolCallResult[]> {
      const settled = new Array<ToolCallResult | undefined>(calls.length);
      const ordinary: number[] = [];
      const terminal: number[] = [];
      calls.forEach((call, index) => {
        (call.name === input.terminalName ? terminal : ordinary).push(index);
      });

      const dispatch = async (index: number): Promise<boolean> => {
        const call = calls[index];
        batch.emit({
          type: "tool_execution_started",
          tool_use_id: call.tool_use_id,
          name: call.name,
          input: call.input,
        });
        const result = await executeCall(call, byName, batch);
        // The batch already settled with synthetic results; drop the
        // straggler so no event lands after run_finished.
        if (isAborted(batch.signal)) return false;
        settled[index] = result;
        batch.emit({
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
        return true;
      };

      let cursor = 0;
      const lane = async (): Promise<void> => {
        while (cursor < ordinary.length && !batch.signal.aborted) {
          const index = ordinary[cursor];
          cursor += 1;
          if (!(await dispatch(index))) return;
        }
      };
      await settledOrAborted(
        Promise.all(
          Array.from(
            { length: Math.min(MAX_TOOL_CONCURRENCY, ordinary.length) },
            () => lane(),
          ),
        ),
        batch.signal,
      );

      for (const index of terminal) {
        if (isAborted(batch.signal)) break;
        if (!(await dispatch(index))) break;
      }

      return calls.map(
        (call, index) => settled[index] ?? errorResult(call.tool_use_id, "interrupted"),
      );
    },
  };
}

async function executeCall(
  call: ToolUseBlock,
  byName: Map<string, BoundTool>,
  batch: ToolBatchContext,
): Promise<ToolCallResult> {
  const tool = byName.get(call.name);
  if (!tool) return errorResult(call.tool_use_id, `tool not found: ${call.name}`);
  try {
    return { tool_use_id: call.tool_use_id, ...(await tool.run(call, batch)) };
  } catch (error) {
    // The pipeline never throws by contract; this keeps a faulting call
    // from cascading into siblings all the same.
    return errorResult(
      call.tool_use_id,
      error instanceof Error ? error.message : String(error),
    );
  }
}

/** String projection for events (results stay structured). */
function projectContent(content: PipelineResult["content"]): string {
  return typeof content === "string" ? content : JSON.stringify(content);
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

/** Resolve when every lane settles or the signal aborts, whichever first. */
async function settledOrAborted(
  lanes: Promise<unknown>,
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
    await Promise.race([lanes, aborted]);
  } finally {
    if (onAbort) signal.removeEventListener("abort", onAbort);
  }
}
