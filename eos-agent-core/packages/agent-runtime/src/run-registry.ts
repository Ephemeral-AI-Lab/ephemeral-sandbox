import type { AgentKind, AgentRunId } from "@eos/contracts";
import type { AgentRunHandle } from "@eos/engine";
import type { AgentRunState } from "@eos/tool";

/** §9 public run row; snake_case: serialized for transports later. */
export interface RunSummary {
  run_id: AgentRunId;
  agent_name: string;
  agent_kind: AgentKind;
  /** `settled` stays session vocabulary. */
  status: "running" | "finished";
  parent?: AgentRunId;
}

interface RunEntry {
  state: AgentRunState;
  handle: AgentRunHandle;
  status: "running" | "finished";
}

/**
 * One typed map of runs. The run facts live exactly once, in the state
 * record; the registry adds only what the record must not hold - the live
 * handle and the registry-level status. Terminal runs stay listed for the
 * process lifetime (eviction is a deferred seam), so transcript reads
 * against finished runs keep working.
 */
export class RunRegistry {
  readonly #runs = new Map<AgentRunId, RunEntry>();

  /** The single registration call; atomic and last in the §4 wiring. */
  add(state: AgentRunState, handle: AgentRunHandle): void {
    this.#runs.set(state.run_id, { state, handle, status: "running" });
  }

  /** Pure bookkeeping, subscribed after the authoritative transcript flush. */
  finish(runId: AgentRunId): void {
    const entry = this.#runs.get(runId);
    if (entry) entry.status = "finished";
  }

  /** `run_id -> transcript path`; the model never supplies a raw path. */
  transcriptPathOf(runId: AgentRunId): string | undefined {
    return this.#runs.get(runId)?.state.transcript_path;
  }

  list(): readonly RunSummary[] {
    return [...this.#runs.values()].map((entry) => ({
      run_id: entry.state.run_id,
      agent_name: entry.state.agent_name,
      agent_kind: entry.state.kind,
      status: entry.status,
      ...(entry.state.parent !== undefined && { parent: entry.state.parent }),
    }));
  }
}
