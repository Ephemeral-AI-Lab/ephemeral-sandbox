import type { ContentBlock, ToolCallResult, ToolSpec } from "@eos/contracts";
import type { AgentEvent } from "./agent-runtime-handle.js";

/** A model-emitted `tool_use` block, the unit of batch dispatch. */
export type ToolUseBlock = Extract<ContentBlock, { type: "tool_use" }>;

/**
 * The engine's ONE piece of tool knowledge - an injected port. Registry,
 * concurrency cap, batch policy, hooks, and the per-call pipeline all live
 * behind it (in `@eos/tool`). The engine keeps only the invariant it cannot
 * delegate: after `executeBatch` returns, any unanswered `tool_use_id` is
 * filled with a synthetic error result so provider-history validity never
 * depends on executor correctness.
 */
export interface ToolExecutor {
  /** Evaluated per turn; `@eos/tool` filters by workspace mode here. */
  specs(): ToolSpec[];
  executeBatch(
    calls: ToolUseBlock[],
    signal: AbortSignal,
    emit: (event: AgentEvent) => void,
  ): Promise<ToolCallResult[]>;
}
