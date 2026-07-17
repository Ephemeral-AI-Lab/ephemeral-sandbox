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
  return fetchObservabilityView<SandboxSnapshot>(sandboxId, "snapshot").then((snapshot) => ({
    sandboxes: [snapshot],
  }));
}

export function fetchObservabilityView<T = unknown>(
  sandboxId: string,
  operation: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return rpc<T>(operation, sandboxScope(sandboxId), args);
}

export function inFlightCount(snapshot: SandboxSnapshot): number {
  return snapshot.workspaces.reduce(
    (total, workspace) => total + workspace.active_namespace_executions.length,
    0,
  );
}

export interface CgroupSeries {
  view: "cgroup";
  scope: string;
  series: ResourceSample[];
  topology: WorkspaceProcessTopology;
}

export interface WorkspaceProcessTopology {
  schema_version: 2;
  available: boolean;
  source: "proc_namespaces" | null;
  error: string | null;
  truncated: boolean;
  warnings: string[];
  workspaces: WorkspaceProcesses[];
}

export interface WorkspaceProcesses {
  workspace_id: string;
  state: "active" | "idle" | "partial";
  holder_pid: number;
  pid_namespace: string | null;
  mount_namespace: string | null;
  processes: WorkspaceProcess[];
}

export interface WorkspaceProcess {
  pid: number;
  namespace_pid: number;
  parent_pid: number;
  name: string;
  state: string;
  kind: "namespace_init" | "process";
  cgroup_memberships: string[];
}

export function fetchCgroup(
  sandboxId: string,
  scope: string,
  windowMs: number,
): Promise<CgroupSeries> {
  return fetchObservabilityView<CgroupSeries>(sandboxId, "cgroup", {
    scope,
    window_ms: windowMs,
  });
}

export interface TraceEvent {
  ts: number;
  trace: string;
  parent: string | null;
  name: string;
  attrs: Record<string, unknown>;
}

export interface TraceSpan {
  ts: number;
  trace: string;
  span: string;
  name: string;
  dur_ms: number | null;
  status: string;
  attrs: Record<string, unknown>;
}

/** An event attached to a span, positioned from the trace start by the API. */
export interface TraceEventNode {
  offset_ms: number;
  event: TraceEvent;
}

export interface TraceNode {
  span: TraceSpan;
  offset_ms: number;
  children: TraceNode[];
  events: TraceEventNode[];
}

export interface TraceResult {
  view: "trace";
  trace: string;
  spans: TraceNode[];
}

export function fetchTrace(sandboxId: string, traceId: string): Promise<TraceResult> {
  return fetchObservabilityView<TraceResult>(sandboxId, "trace", {
    trace_id: traceId,
  });
}

export interface EventsResult {
  view: "events";
  events: TraceEvent[];
}

export function fetchEvents(
  sandboxId: string,
  filters: { name?: string; sinceMs?: number; lastN?: number },
): Promise<EventsResult> {
  const args: Record<string, unknown> = {};
  if (filters.name) args["name"] = filters.name;
  if (filters.sinceMs !== undefined) args["since_ms"] = filters.sinceMs;
  if (filters.lastN !== undefined) args["last_n"] = filters.lastN;
  return fetchObservabilityView<EventsResult>(sandboxId, "events", args);
}

export interface StackLayer {
  layer_id: string;
  bytes: number;
  leased_by_workspaces: number;
  booked_by: string[];
}

export interface LayerStackResult {
  view: "layerstack";
  manifest_version: number;
  root_hash: string;
  active_lease_count: number;
  total_bytes: number;
  layers: StackLayer[];
}

export function fetchLayerStack(sandboxId: string): Promise<LayerStackResult> {
  return fetchObservabilityView<LayerStackResult>(sandboxId, "layerstack");
}

export interface LayerStackWorkspaceMount {
  layer_id: string;
  shared_with: string[];
}

export interface LayerStackWorkspaceResult {
  view: "layerstack";
  workspace: string;
  mounts: LayerStackWorkspaceMount[];
  upper_bytes: number | null;
}

export function fetchLayerStackWorkspace(
  sandboxId: string,
  workspaceId: string,
): Promise<LayerStackWorkspaceResult> {
  return fetchObservabilityView<LayerStackWorkspaceResult>(sandboxId, "layerstack", {
    workspace_id: workspaceId,
  });
}

export type LayerStackLayerEntryKind =
  | "file"
  | "directory"
  | "symlink"
  | "delete"
  | "opaque_dir";

export interface LayerStackLayerEntry {
  path: string;
  kind: LayerStackLayerEntryKind;
}

export interface LayerStackLayerResult {
  view: "layerstack";
  layer_id: string;
  entries: LayerStackLayerEntry[];
  truncated: boolean;
}

export function fetchLayerStackLayer(
  sandboxId: string,
  layerId: string,
): Promise<LayerStackLayerResult> {
  return fetchObservabilityView<LayerStackLayerResult>(sandboxId, "layerstack", {
    layer_id: layerId,
  });
}
