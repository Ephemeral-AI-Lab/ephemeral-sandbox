import {
  zodIssueSummary,
  type AgentRunSnapshot,
  type BackgroundSessionSnapshot,
  type JsonObject,
  type ToolCallResult,
} from "@eos/contracts";
import type { ToolUseBlock } from "@eos/engine";

import type {
  ToolCallMeta,
  ToolDefinition,
  ToolOutcome,
} from "./contract.js";
import type {
  HookAdvisoryRequirement,
  HookEvent,
  HookPayload,
} from "./hooks/protocol.js";
import type { HookEngine, HookRunSummary } from "./hooks/runner.js";

/**
 * Everything the pipeline owns about one settled call. The batch executor
 * constructs the `ToolCallResult` - it must pair the `tool_use_id`.
 */
export type PipelineResult = Omit<ToolCallResult, "tool_use_id">;

/** Run-level dependencies closed over at executor build. */
export interface BindToolDeps {
  hooks: HookEngine;
  advisoryRequirement: HookAdvisoryRequirement;
  hookPayloadFacts?: () => HookPayloadFacts;
}

export interface HookPayloadFacts {
  background_sessions?: readonly BackgroundSessionSnapshot[];
}

/** One definition bound through the pipeline; dispatched by the executor. */
export interface BoundTool {
  definition: ToolDefinition;
  run(
    call: ToolUseBlock,
    run: AgentRunSnapshot,
    signal: AbortSignal,
  ): Promise<PipelineResult>;
}

/**
 * Bind one definition to the per-call pipeline: abort check -> isolated
 * guard -> parse -> PreToolUse -> execute -> PostToolUse(/Failure) ->
 * stamping. Never throws; every path returns a result the executor can
 * record. Timing brackets `execute()` only - pre-execution rejections
 * stamp both times with the rejection instant, and slow hooks never
 * masquerade as slow tools.
 */
export function bindTool(definition: ToolDefinition, deps: BindToolDeps): BoundTool {
  const run = async (
    call: ToolUseBlock,
    runSnapshot: AgentRunSnapshot,
    signal: AbortSignal,
  ): Promise<PipelineResult> => {
    const meta: ToolCallMeta = Object.freeze({
      tool_use_id: call.tool_use_id,
      tool_name: definition.name,
      run: runSnapshot,
    });
    const warnings: string[] = [];
    // Decision 04.5/11: hook context rides the result under
    // `metadata.hook_contexts`; the engine loop is its only publisher.
    const contexts: string[] = [];
    const absorb = (summary: HookRunSummary): void => {
      warnings.push(...summary.warnings);
      contexts.push(...summary.additionalContexts);
    };
    const rejected = (content: string): PipelineResult => {
      const at = Date.now();
      return stamp(definition, { content, isError: true }, at, at, warnings, contexts);
    };

    // Defense in depth: the executor already stops dispatching on abort.
    if (signal.aborted) return rejected("interrupted");

    if (runSnapshot.workspace.is_isolated && !definition.availableInIsolatedWorkspace) {
      return rejected(
        `tool ${definition.name} is not available while the workspace is isolated`,
      );
    }

    const parsed = definition.input.safeParse(call.input);
    if (!parsed.success) {
      return rejected(
        `invalid input for ${definition.name}: ${zodIssueSummary(parsed.error)}`,
      );
    }
    let input: unknown = parsed.data;
    let wireInput: JsonObject = call.input;
    // Reads the CURRENT wireInput, so post hooks see an applied update.
    const hookPayload = (
      event: HookEvent,
      extra: Partial<Pick<HookPayload, "tool_response" | "error">> = {},
    ): HookPayload => ({
      event,
      tool_name: definition.name,
      tool_input: wireInput,
      tool_use_id: meta.tool_use_id,
      run: meta.run,
      advisory_requirement: deps.advisoryRequirement,
      ...deps.hookPayloadFacts?.(),
      ...extra,
    });

    const pre = await deps.hooks.run(hookPayload("PreToolUse"), signal);
    absorb(pre);
    if (pre.decision === "deny") {
      return rejected(pre.reason ?? `PreToolUse hook denied ${definition.name}`);
    }
    if (pre.updatedInput) {
      // A hook can rewrite input but cannot smuggle an invalid shape past
      // the tool's own schema.
      const reparsed = definition.input.safeParse(pre.updatedInput);
      if (!reparsed.success) {
        return rejected(
          `hook updatedInput rejected by ${definition.name} schema: ${zodIssueSummary(reparsed.error)}`,
        );
      }
      input = reparsed.data;
      wireInput = pre.updatedInput;
    }

    const startedAt = Date.now();
    let outcome: ToolOutcome;
    try {
      outcome = await definition.execute(input, { meta, signal });
    } catch (error) {
      const endedAt = Date.now();
      const message = error instanceof Error ? error.message : String(error);
      absorb(
        await deps.hooks.run(hookPayload("PostToolUseFailure", { error: message }), signal),
      );
      return stamp(
        definition,
        { content: message, isError: true },
        startedAt,
        endedAt,
        warnings,
        contexts,
      );
    }
    const endedAt = Date.now();

    absorb(
      await deps.hooks.run(
        hookPayload("PostToolUse", { tool_response: projectContent(outcome.content) }),
        signal,
      ),
    );

    return stamp(definition, outcome, startedAt, endedAt, warnings, contexts);
  };
  return { definition, run };
}

/** String projection for events and hook payloads (results stay structured). */
export function projectContent(content: ToolOutcome["content"]): string {
  return typeof content === "string" ? content : JSON.stringify(content);
}

/**
 * The pipeline's stamping: `is_terminal = definition.isTerminal && !isError`
 * (a failed submission can never terminate a run) plus the execute-only
 * clock and the accumulated hook warnings and contexts (the engine loop
 * publishes each `hook_contexts` entry as a `hook_context` notification).
 */
function stamp(
  definition: ToolDefinition,
  outcome: ToolOutcome,
  startedAt: number,
  endedAt: number,
  warnings: string[],
  contexts: string[],
): PipelineResult {
  const isError = outcome.isError ?? false;
  const hookFacts: JsonObject = {
    ...(warnings.length > 0 && { hook_warnings: warnings }),
    ...(contexts.length > 0 && { hook_contexts: contexts }),
  };
  const metadata =
    Object.keys(hookFacts).length > 0
      ? { ...outcome.metadata, ...hookFacts }
      : outcome.metadata;
  const result: PipelineResult = {
    content: outcome.content,
    is_error: isError,
    is_terminal: definition.isTerminal && !isError,
    tool_start_time: startedAt,
    tool_end_time: endedAt,
  };
  if (metadata !== undefined) result.metadata = metadata;
  return result;
}
