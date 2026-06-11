import type {
  AgentKind,
  AgentRunId,
  AgentRunSnapshot,
  SandboxId,
} from "@eos/contracts";

/**
 * The per-run metadata record, assembled once by the composition root.
 * Data only - a port or service in here would be the rejected ambient
 * `ToolRuntime`. Everything except `workspace.isIsolated` is readonly;
 * that one cell's writers are the sandbox family's workspace-mode tools.
 * Tools never receive this record at call time - `ToolCallMeta` nests its
 * frozen `AgentRunSnapshot` instead.
 */
export interface AgentRunState {
  readonly run_id: AgentRunId;
  readonly kind: AgentKind;
  readonly parent?: AgentRunId;
  /** The profile name the run was started under (`parent` stays the caller link). */
  readonly agent_name: string;
  readonly sandbox_id: SandboxId;
  readonly transcript_path: string;
  /** The ONE mutable cell; any future mutable field needs a named writer. */
  readonly workspace: { isIsolated: boolean };
}

/**
 * Spread + freeze of the whole record, with the mutable cell collapsed to
 * its value at snapshot time. The executor takes one snapshot per batch,
 * so a mid-batch mode flip never leaks into queued siblings.
 */
export function snapshotRunState(state: AgentRunState): AgentRunSnapshot {
  const { workspace, ...facts } = state;
  return Object.freeze({
    ...facts,
    workspace: Object.freeze({ is_isolated: workspace.isIsolated }),
  });
}
