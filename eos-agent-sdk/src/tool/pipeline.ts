import {
  zodIssueSummary,
  type AgentRunId,
  type JsonObject,
  type ToolCallResult,
  type ToolSpec,
} from "../contracts/index.js";
import type { BackgroundTaskSupervisor } from "../background/index.js";
import type { ToolBatchContext, ToolUseBlock } from "../engine/index.js";
import type { Notifier } from "../notification/index.js";
import { z } from "zod";

import type { ToolCallContext, ToolDefinition, ToolResult } from "./contract.js";
import type { HookEngine, ToolCallFacts } from "./hooks.js";

/**
 * Everything the pipeline owns about one settled call. The batch executor
 * constructs the `ToolCallResult` - it must pair the `tool_use_id`.
 */
export type PipelineResult = Omit<ToolCallResult, "tool_use_id">;

/** Run-scoped dependencies closed over when an executor is built. */
export interface RunScope {
  runId: AgentRunId;
  backgroundTaskSupervisor: BackgroundTaskSupervisor;
  notifier: Notifier;
  hooks: HookEngine;
}

/** One definition bound through the pipeline; dispatched by the executor. */
export interface BoundTool {
  name: string;
  spec: ToolSpec;
  run(call: ToolUseBlock, batch: ToolBatchContext): Promise<PipelineResult>;
}

/** Derive the wire declaration from the Zod input contract. */
function toolSpec(definition: ToolDefinition): ToolSpec {
  return {
    name: definition.name,
    description: definition.description,
    // JSON Schema output is JSON by construction.
    input_schema: z.toJSONSchema(definition.input) as JsonObject,
  };
}

/** Frozen per-call facts shared by pre and post hooks. */
export function callFacts(
  runId: AgentRunId,
  call: ToolUseBlock,
  toolName: string,
): ToolCallFacts {
  return Object.freeze({
    runId,
    toolUseId: call.tool_use_id,
    toolName,
    input: call.input,
  });
}

/** A settled rejection with both clocks at the rejection instant. */
export function rejectedResult(content: string): PipelineResult {
  const at = Date.now();
  return {
    content,
    is_error: true,
    is_terminal: false,
    tool_start_time: at,
    tool_end_time: at,
  };
}

/**
 * Bind one definition to the per-call pipeline: abort check -> parse ->
 * preToolUse -> execute -> postToolUse -> stamping. Never throws; every
 * path returns a result the executor can record. Timing brackets
 * `execute()` only - pre-execution rejections stamp both times with the
 * rejection instant, and slow hooks never masquerade as slow tools.
 */
export function bindTool(definition: ToolDefinition, scope: RunScope): BoundTool {
  const run = async (
    call: ToolUseBlock,
    batch: ToolBatchContext,
  ): Promise<PipelineResult> => {
    // Defense in depth: the executor already stops dispatching on abort.
    if (batch.signal.aborted) return rejectedResult("interrupted");

    const parsed = definition.input.safeParse(call.input);
    if (!parsed.success) {
      return rejectedResult(
        `invalid input for ${definition.name}: ${zodIssueSummary(parsed.error)}`,
      );
    }

    const facts = callFacts(scope.runId, call, definition.name);
    const pre = await scope.hooks.preToolUse(facts);
    if (pre.decision === "deny") return rejectedResult(pre.reason);

    const ctx: ToolCallContext = {
      runId: scope.runId,
      toolUseId: call.tool_use_id,
      signal: batch.signal,
      llmMessages: batch.llmMessages,
      backgroundTaskSupervisor: scope.backgroundTaskSupervisor,
      notifier: scope.notifier,
    };
    const startedAt = Date.now();
    let result: ToolResult;
    try {
      result = await definition.execute(parsed.data, ctx);
    } catch (error) {
      result = { error: error instanceof Error ? error.message : String(error) };
    }
    const endedAt = Date.now();

    const post = await scope.hooks.postToolUse(facts, result);
    if (post.decision === "deny") result = { error: post.reason };

    return stamp(result, startedAt, endedAt);
  };
  return { name: definition.name, spec: toolSpec(definition), run };
}

/**
 * The pipeline's stamping for ordinary tools: errors are facts mapped by
 * the pipeline, never tool claims, and `is_terminal` is always false —
 * only the terminal binding stamps true.
 */
function stamp(
  result: ToolResult,
  startedAt: number,
  endedAt: number,
): PipelineResult {
  if ("error" in result) {
    return {
      content: result.error,
      is_error: true,
      is_terminal: false,
      tool_start_time: startedAt,
      tool_end_time: endedAt,
    };
  }
  return {
    content: result.output,
    is_error: false,
    is_terminal: false,
    tool_start_time: startedAt,
    tool_end_time: endedAt,
    ...(result.metadata !== undefined && { metadata: result.metadata }),
  };
}
