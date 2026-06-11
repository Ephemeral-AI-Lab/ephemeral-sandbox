import type {
  AgentRunSnapshot,
  JsonObject,
  JsonValue,
  ToolSpec,
  ToolUseId,
} from "@eos/contracts";
import { z } from "zod";

/** Branded tool name; `defineTool` is the only mint site. */
export const ToolNameSchema = z.string().min(1).brand<"ToolName">();
export type ToolName = z.infer<typeof ToolNameSchema>;

/**
 * What a tool execution yields. Deliberately small: `is_terminal` and the
 * timing stamps are facts about the execution, owned by the pipeline -
 * never claims by the tool.
 */
export interface ToolOutcome {
  /** Model-facing; string or structured (stringified once, by the engine). */
  content: JsonValue;
  /** Defaults to false. */
  isError?: boolean;
  /** Transcript/observability only. */
  metadata?: JsonObject;
}

/**
 * Serializable per-call facts; built once per call, frozen, and shared by
 * pre-hooks, `execute`, and post-hooks (command hooks eat this as JSON).
 * `run` is the contracts-level `AgentRunSnapshot`, built by spread +
 * freeze over `AgentRunState`.
 */
export interface ToolCallMeta {
  readonly tool_use_id: ToolUseId;
  readonly tool_name: ToolName;
  readonly run: AgentRunSnapshot;
}

/**
 * What `execute()` receives: the frozen facts plus the one live handle.
 * Services are NOT here - handlers close over their own service at
 * construction, so neither tools nor hooks ever see a port bag.
 */
export interface ToolCallContext {
  readonly meta: ToolCallMeta;
  readonly signal: AbortSignal;
}

/**
 * The authoring surface. Build instances through `defineTool`, which
 * centralizes the fail-closed flag defaults and derives `spec`.
 */
export interface ToolDefinition<I = unknown> {
  readonly name: ToolName;
  readonly description: string;
  /** Input contract; also the wire spec source via `z.toJSONSchema`. */
  readonly input: z.ZodType<I>;
  /** Submissions only: a non-error result finishes the run. */
  readonly isTerminal: boolean;
  /** Batch policy: this call must be the only one in its batch. */
  readonly isBatchExecutionForbidden: boolean;
  /** Sandbox family only: callable while the workspace is isolated. */
  readonly availableInIsolatedWorkspace: boolean;
  /** Advisory-gated tools need an exact advisor pass before execution. */
  readonly isAdvisoryRequired: boolean;
  /** Prompt used by `ask_advisor`; present only when advisory is required. */
  readonly advisorPrompt?: string;
  /** Wire declaration derived once by `defineTool`. */
  readonly spec: ToolSpec;
  execute(input: I, ctx: ToolCallContext): Promise<ToolOutcome>;
}
