//! Daemon operation names owned by the protocol crate.
//!
//! The live `eosd` dispatcher registers these exact strings, and protocol
//! clients should import them from here instead of duplicating string literals.

/// Runtime readiness probe.
pub const API_RUNTIME_READY: &str = "api.runtime.ready";
/// Invocation heartbeat.
pub const API_V1_HEARTBEAT: &str = "api.v1.heartbeat";
/// Cancel an in-flight invocation.
pub const API_V1_CANCEL: &str = "api.v1.cancel";
/// Count in-flight invocations.
pub const API_V1_INFLIGHT_COUNT: &str = "api.v1.inflight_count";
/// LayerStack/storage metrics.
pub const API_LAYER_METRICS: &str = "api.layer_metrics";
/// Ensure a workspace base binding exists.
pub const API_ENSURE_WORKSPACE_BASE: &str = "api.ensure_workspace_base";
/// Build or rebuild a workspace base binding.
pub const API_BUILD_WORKSPACE_BASE: &str = "api.build_workspace_base";
/// Materialize LayerStack state into the bound workspace.
pub const API_COMMIT_TO_WORKSPACE: &str = "api.commit_to_workspace";
/// Commit a LayerStack snapshot into the bound workspace's durable Git repo.
pub const API_COMMIT_TO_GIT: &str = "api.commit_to_git";
/// Inspect the workspace binding for a layer stack root.
pub const API_WORKSPACE_BINDING: &str = "api.workspace_binding";
/// Pull audit events after a cursor.
pub const API_AUDIT_PULL: &str = "api.audit.pull";
/// Snapshot audit ring metadata.
pub const API_AUDIT_SNAPSHOT: &str = "api.audit.snapshot";
/// Reset the audit floor when daemon-side test gate allows it.
pub const API_AUDIT_RESET_FLOOR: &str = "api.audit.reset_floor";
/// Direct LayerStack read.
pub const API_V1_READ_FILE: &str = "api.v1.read_file";
/// Direct OCC-gated write.
pub const API_V1_WRITE_FILE: &str = "api.v1.write_file";
/// Direct OCC-gated edit.
pub const API_V1_EDIT_FILE: &str = "api.v1.edit_file";
/// Ensure a plugin service is available.
pub const API_PLUGIN_ENSURE: &str = "api.plugin.ensure";
/// Inspect plugin service status.
pub const API_PLUGIN_STATUS: &str = "api.plugin.status";
/// Enter isolated workspace mode.
pub const API_ISOLATED_WORKSPACE_ENTER: &str = "api.isolated_workspace.enter";
/// Exit isolated workspace mode.
pub const API_ISOLATED_WORKSPACE_EXIT: &str = "api.isolated_workspace.exit";
/// Inspect isolated workspace status.
pub const API_ISOLATED_WORKSPACE_STATUS: &str = "api.isolated_workspace.status";
/// List open isolated workspaces.
pub const API_ISOLATED_WORKSPACE_LIST_OPEN: &str = "api.isolated_workspace.list_open";
/// Test-only isolated workspace reset hook.
pub const API_ISOLATED_WORKSPACE_TEST_RESET: &str = "api.isolated_workspace.test_reset";
/// Start or poll a command session.
pub const API_V1_EXEC_COMMAND: &str = "api.v1.exec_command";
/// Write stdin to a command session.
pub const API_V1_WRITE_STDIN: &str = "api.v1.write_stdin";
/// Read command-session progress without writing stdin.
pub const API_V1_COMMAND_READ_PROGRESS: &str = "api.v1.command.read_progress";
/// Cancel a command session.
pub const API_V1_COMMAND_CANCEL: &str = "api.v1.command.cancel";
/// Collect completed command-session notifications.
pub const API_V1_COMMAND_COLLECT_COMPLETED: &str = "api.v1.command.collect_completed";
/// Count live command sessions.
pub const API_V1_COMMAND_SESSION_COUNT: &str = "api.v1.command_session_count";
/// Cancel every workspace run owned by one caller (`caller_id == agent_run_id`):
/// discards the caller's command session(s) and exits its isolated workspace if
/// open. The agent-core per-run cancellation RPC.
pub const API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER: &str =
    "api.v1.cancel_workspace_runs_by_caller_id";
/// Cancel every workspace run in the sandbox (whole-sandbox sweep backstop):
/// discards all command sessions, exits all isolated callers, reaps orphans.
pub const API_V1_CANCEL_WORKSPACE_RUNS: &str = "api.v1.cancel_workspace_runs";

/// Built-in daemon ops expected to be available over the wire.
pub const BUILTIN_DAEMON_OPS: &[&str] = &[
    API_RUNTIME_READY,
    API_V1_HEARTBEAT,
    API_V1_CANCEL,
    API_V1_INFLIGHT_COUNT,
    API_LAYER_METRICS,
    API_ENSURE_WORKSPACE_BASE,
    API_BUILD_WORKSPACE_BASE,
    API_COMMIT_TO_WORKSPACE,
    API_COMMIT_TO_GIT,
    API_WORKSPACE_BINDING,
    API_AUDIT_PULL,
    API_AUDIT_SNAPSHOT,
    API_AUDIT_RESET_FLOOR,
    API_V1_READ_FILE,
    API_V1_WRITE_FILE,
    API_V1_EDIT_FILE,
    API_PLUGIN_ENSURE,
    API_PLUGIN_STATUS,
    API_ISOLATED_WORKSPACE_ENTER,
    API_ISOLATED_WORKSPACE_EXIT,
    API_ISOLATED_WORKSPACE_STATUS,
    API_ISOLATED_WORKSPACE_LIST_OPEN,
    API_ISOLATED_WORKSPACE_TEST_RESET,
    API_V1_EXEC_COMMAND,
    API_V1_WRITE_STDIN,
    API_V1_COMMAND_READ_PROGRESS,
    API_V1_COMMAND_CANCEL,
    API_V1_COMMAND_COLLECT_COMPLETED,
    API_V1_COMMAND_SESSION_COUNT,
    API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER,
    API_V1_CANCEL_WORKSPACE_RUNS,
];
