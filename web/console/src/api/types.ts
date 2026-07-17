export type SandboxState =
  | "creating"
  | "ready"
  | "stopping"
  | "stopped"
  | "failed";

export interface Endpoint {
  host: string;
  port: number;
}

export interface SharedBase {
  source: string;
  target: string;
  root_hash: string;
  readonly: boolean;
}

export interface SandboxRecord {
  id: string;
  workspace_root: string;
  state: SandboxState;
  daemon: Endpoint | null;
  daemon_http: Endpoint | null;
  shared_base: SharedBase | null;
  /** Absent only while connected to a pre-revision manager. */
  activity_revision?: number;
}

export interface SandboxList {
  sandboxes: SandboxRecord[];
}

export type CommandStatus =
  | "running"
  | "ok"
  | "error"
  | "timed_out"
  | "cancelled";

export interface CommandOutput {
  status: CommandStatus;
  exit_code: number | null;
  wall_time_seconds: number;
  command_total_time_seconds: number;
  start_offset: number;
  end_offset: number;
  total_lines: number;
  original_token_count: number;
  output: string;
  command_session_id?: string;
  workspace_session_id?: string;
  publish_rejected?: boolean;
  publish_reject_class?: string;
}

export interface WorkspaceSessionCreated {
  workspace_session_id: string;
  network_profile: "shared" | "isolated";
  finalize_policy: "no_op";
}

export interface WorkspaceSessionDestroyed {
  workspace_session_id: string;
  destroyed: true;
  evicted_upperdir_bytes: number;
}
