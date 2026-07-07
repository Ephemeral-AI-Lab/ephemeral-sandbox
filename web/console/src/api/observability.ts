import { rpc, sandboxScope, systemScope } from "@/api/rpc";

export interface ResourceSample {
  ts: number;
  sample_delta_ms: number;
  metrics: Record<string, number | boolean | string>;
  deltas: Record<string, number>;
}

export interface ResourceSeries {
  latest: ResourceSample | null;
  history: ResourceSample[];
}

export interface ActiveExecution {
  namespace_execution_id: string;
  operation: string;
  lifecycle_state: string;
}

export interface WorkspaceSnapshot {
  workspace_id: string;
  lifecycle_state: string;
  network_profile: "shared" | "isolated";
  layers: { base_root_hash: string; layer_count: number };
  namespace_fd_count: number;
  resources: ResourceSeries;
  active_namespace_executions: ActiveExecution[];
}

export interface SandboxSnapshot {
  sandbox_id: string;
  lifecycle_state: string;
  availability: string;
  sampled_at_unix_ms: number;
  errors: string[];
  daemon: { daemon_pid: number; runtime_dir: string } | null;
  resources: ResourceSeries;
  workspaces: WorkspaceSnapshot[];
  stack: { layer_count: number; layers_bytes: number; active_leases: number };
}

export interface SnapshotResult {
  sandboxes: SandboxSnapshot[];
}

export function fetchFleetSnapshot(): Promise<SnapshotResult> {
  return rpc<SnapshotResult>("snapshot", systemScope);
}

export function fetchSandboxSnapshot(sandboxId: string): Promise<SnapshotResult> {
  return rpc<SnapshotResult>("snapshot", systemScope, { sandbox_id: sandboxId });
}

export function fetchObservabilityView<T = unknown>(
  sandboxId: string,
  view: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return rpc<T>("get_observability", sandboxScope(sandboxId), { view, ...args });
}

export function inFlightCount(snapshot: SandboxSnapshot): number {
  return snapshot.workspaces.reduce(
    (total, workspace) => total + workspace.active_namespace_executions.length,
    0,
  );
}
