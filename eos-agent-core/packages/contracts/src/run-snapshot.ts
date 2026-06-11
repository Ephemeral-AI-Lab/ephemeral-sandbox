import type { AgentKind } from "./agents.js";
import type { AgentRunId, SandboxId } from "./ids.js";

/**
 * Frozen, fully serializable copy of the tool layer's `AgentRunState`:
 * same fields, with the one mutable cell collapsed to the batch's
 * snapshot. Carried by every operator-script payload (tool hooks and
 * notification trigger rules), so it lives with the shared contracts.
 */
export interface AgentRunSnapshot {
  readonly run_id: AgentRunId;
  readonly kind: AgentKind;
  readonly parent?: AgentRunId;
  readonly agent_name: string;
  readonly sandbox_id: SandboxId;
  readonly transcript_path: string;
  /** Batch-scoped snapshot: a mid-batch flip never leaks into siblings. */
  readonly workspace: { readonly is_isolated: boolean };
}

/**
 * Serialized background-session row in operator-script payloads (tool
 * hooks and notification trigger rules). snake_case: crosses the process
 * boundary.
 */
export interface BackgroundSessionSnapshot {
  type: string;
  id: string;
  status: "running" | "completed" | "failed" | "cancelled";
  /** ISO-8601 registration time. */
  started_at: string;
  summary?: string;
  description?: string;
}
