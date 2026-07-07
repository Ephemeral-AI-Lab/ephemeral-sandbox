import type { SandboxList } from "@/api/types";
import { inFlightCount, type SnapshotResult } from "@/api/observability";
import { formatBytes } from "@/lib/format";

/**
 * Client-side aggregation of the per-sandbox snapshot list — the no-arg
 * snapshot returns `{sandboxes: [...]}` with nothing pre-aggregated.
 */
export function FleetSummaryBar({
  list,
  snapshot,
}: {
  list: SandboxList | undefined;
  snapshot: SnapshotResult | undefined;
}) {
  const records = list?.sandboxes ?? [];
  const byState = new Map<string, number>();
  for (const record of records) {
    byState.set(record.state, (byState.get(record.state) ?? 0) + 1);
  }
  const snapshots = snapshot?.sandboxes ?? [];
  const executions = snapshots.reduce((total, entry) => total + inFlightCount(entry), 0);
  const layers = snapshots.reduce((total, entry) => total + entry.stack.layer_count, 0);
  const memory = snapshots.reduce((total, entry) => {
    const mem = entry.resources.latest?.metrics["mem_cur"];
    return typeof mem === "number" ? total + mem : total;
  }, 0);

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-line bg-surface px-4 py-2 text-[13px] text-ink-mid">
      <span>
        Fleet: <span className="font-medium text-ink">{records.length}</span>{" "}
        {records.length === 1 ? "sandbox" : "sandboxes"}
      </span>
      {[...byState.entries()].map(([state, count]) => (
        <span key={state}>
          {count} {state}
        </span>
      ))}
      <span>
        {executions} running {executions === 1 ? "command" : "commands"}
      </span>
      <span>Σ mem {formatBytes(memory)}</span>
      <span>Σ {layers} layers</span>
    </div>
  );
}
