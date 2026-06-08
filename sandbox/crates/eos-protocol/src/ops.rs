//! Daemon operation names owned by the protocol crate.
//!
//! The live `eosd` dispatcher registers these exact strings, and protocol
//! clients should import them from here instead of duplicating string literals.

/// Functional owner for a built-in daemon op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OpFamily {
    /// Runtime readiness, heartbeat, cancellation, and in-flight accounting.
    Control,
    /// LayerStack base, metrics, and checkpoint materialization.
    Checkpoint,
    /// Audit ring pull/snapshot/reset operations.
    Audit,
    /// Shared workspace file read/write/edit operations.
    Files,
    /// Plugin package, service, and dynamic dispatch operations.
    Plugins,
    /// Isolated workspace lifecycle and status operations.
    IsolatedWorkspace,
    /// Command-session lifecycle, IO, and completion operations.
    CommandSession,
    /// Caller-keyed or whole-sandbox workspace-run cleanup operations.
    WorkspaceRun,
}

/// One built-in daemon operation in the wire protocol catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum BuiltinDaemonOp {
    /// `api.runtime.ready`
    RuntimeReady,
    /// `api.v1.heartbeat`
    InvocationHeartbeat,
    /// `api.v1.cancel`
    InvocationCancel,
    /// `api.v1.inflight_count`
    InflightCount,
    /// `api.layer_metrics`
    LayerMetrics,
    /// `api.ensure_workspace_base`
    EnsureWorkspaceBase,
    /// `api.build_workspace_base`
    BuildWorkspaceBase,
    /// `api.commit_to_workspace`
    CommitToWorkspace,
    /// `api.commit_to_git`
    CommitToGit,
    /// `api.workspace_binding`
    WorkspaceBinding,
    /// `api.audit.pull`
    AuditPull,
    /// `api.audit.snapshot`
    AuditSnapshot,
    /// `api.audit.reset_floor`
    AuditResetFloor,
    /// `api.v1.read_file`
    ReadFile,
    /// `api.v1.write_file`
    WriteFile,
    /// `api.v1.edit_file`
    EditFile,
    /// `api.plugin.ensure`
    PluginEnsure,
    /// `api.plugin.status`
    PluginStatus,
    /// `api.isolated_workspace.enter`
    IsolatedWorkspaceEnter,
    /// `api.isolated_workspace.exit`
    IsolatedWorkspaceExit,
    /// `api.isolated_workspace.status`
    IsolatedWorkspaceStatus,
    /// `api.isolated_workspace.list_open`
    IsolatedWorkspaceListOpen,
    /// `api.isolated_workspace.test_reset`
    IsolatedWorkspaceTestReset,
    /// `api.v1.exec_command`
    ExecCommand,
    /// `api.v1.write_stdin`
    WriteStdin,
    /// `api.v1.command.read_progress`
    CommandReadProgress,
    /// `api.v1.command.cancel`
    CommandCancel,
    /// `api.v1.command.collect_completed`
    CommandCollectCompleted,
    /// `api.v1.command_session_count`
    CommandSessionCount,
    /// `api.v1.cancel_workspace_runs_by_caller_id`
    CancelWorkspaceRunsByCaller,
    /// `api.v1.cancel_workspace_runs`
    CancelWorkspaceRuns,
}

impl BuiltinDaemonOp {
    /// Verbatim wire string for this op.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::RuntimeReady => "api.runtime.ready",
            Self::InvocationHeartbeat => "api.v1.heartbeat",
            Self::InvocationCancel => "api.v1.cancel",
            Self::InflightCount => "api.v1.inflight_count",
            Self::LayerMetrics => "api.layer_metrics",
            Self::EnsureWorkspaceBase => "api.ensure_workspace_base",
            Self::BuildWorkspaceBase => "api.build_workspace_base",
            Self::CommitToWorkspace => "api.commit_to_workspace",
            Self::CommitToGit => "api.commit_to_git",
            Self::WorkspaceBinding => "api.workspace_binding",
            Self::AuditPull => "api.audit.pull",
            Self::AuditSnapshot => "api.audit.snapshot",
            Self::AuditResetFloor => "api.audit.reset_floor",
            Self::ReadFile => "api.v1.read_file",
            Self::WriteFile => "api.v1.write_file",
            Self::EditFile => "api.v1.edit_file",
            Self::PluginEnsure => "api.plugin.ensure",
            Self::PluginStatus => "api.plugin.status",
            Self::IsolatedWorkspaceEnter => "api.isolated_workspace.enter",
            Self::IsolatedWorkspaceExit => "api.isolated_workspace.exit",
            Self::IsolatedWorkspaceStatus => "api.isolated_workspace.status",
            Self::IsolatedWorkspaceListOpen => "api.isolated_workspace.list_open",
            Self::IsolatedWorkspaceTestReset => "api.isolated_workspace.test_reset",
            Self::ExecCommand => "api.v1.exec_command",
            Self::WriteStdin => "api.v1.write_stdin",
            Self::CommandReadProgress => "api.v1.command.read_progress",
            Self::CommandCancel => "api.v1.command.cancel",
            Self::CommandCollectCompleted => "api.v1.command.collect_completed",
            Self::CommandSessionCount => "api.v1.command_session_count",
            Self::CancelWorkspaceRunsByCaller => "api.v1.cancel_workspace_runs_by_caller_id",
            Self::CancelWorkspaceRuns => "api.v1.cancel_workspace_runs",
        }
    }

    /// Functional owner for this op.
    #[must_use]
    pub const fn family(self) -> OpFamily {
        match self {
            Self::RuntimeReady
            | Self::InvocationHeartbeat
            | Self::InvocationCancel
            | Self::InflightCount => OpFamily::Control,
            Self::LayerMetrics
            | Self::EnsureWorkspaceBase
            | Self::BuildWorkspaceBase
            | Self::CommitToWorkspace
            | Self::CommitToGit
            | Self::WorkspaceBinding => OpFamily::Checkpoint,
            Self::AuditPull | Self::AuditSnapshot | Self::AuditResetFloor => OpFamily::Audit,
            Self::ReadFile | Self::WriteFile | Self::EditFile => OpFamily::Files,
            Self::PluginEnsure | Self::PluginStatus => OpFamily::Plugins,
            Self::IsolatedWorkspaceEnter
            | Self::IsolatedWorkspaceExit
            | Self::IsolatedWorkspaceStatus
            | Self::IsolatedWorkspaceListOpen
            | Self::IsolatedWorkspaceTestReset => OpFamily::IsolatedWorkspace,
            Self::ExecCommand
            | Self::WriteStdin
            | Self::CommandReadProgress
            | Self::CommandCancel
            | Self::CommandCollectCompleted
            | Self::CommandSessionCount => OpFamily::CommandSession,
            Self::CancelWorkspaceRunsByCaller | Self::CancelWorkspaceRuns => OpFamily::WorkspaceRun,
        }
    }

    /// Whether this op may change daemon, workspace, or process state.
    #[must_use]
    pub const fn mutates_state(self) -> bool {
        match self {
            Self::RuntimeReady
            | Self::InflightCount
            | Self::LayerMetrics
            | Self::WorkspaceBinding
            | Self::AuditPull
            | Self::AuditSnapshot
            | Self::ReadFile
            | Self::PluginStatus
            | Self::IsolatedWorkspaceStatus
            | Self::IsolatedWorkspaceListOpen
            | Self::CommandReadProgress
            | Self::CommandSessionCount => false,
            Self::InvocationHeartbeat
            | Self::InvocationCancel
            | Self::EnsureWorkspaceBase
            | Self::BuildWorkspaceBase
            | Self::CommitToWorkspace
            | Self::CommitToGit
            | Self::AuditResetFloor
            | Self::WriteFile
            | Self::EditFile
            | Self::PluginEnsure
            | Self::IsolatedWorkspaceEnter
            | Self::IsolatedWorkspaceExit
            | Self::IsolatedWorkspaceTestReset
            | Self::ExecCommand
            | Self::WriteStdin
            | Self::CommandCancel
            | Self::CommandCollectCompleted
            | Self::CancelWorkspaceRunsByCaller
            | Self::CancelWorkspaceRuns => true,
        }
    }

    /// Whether this op is a daemon-side test hook.
    #[must_use]
    pub const fn test_only(self) -> bool {
        match self {
            Self::IsolatedWorkspaceTestReset => true,
            Self::RuntimeReady
            | Self::InvocationHeartbeat
            | Self::InvocationCancel
            | Self::InflightCount
            | Self::LayerMetrics
            | Self::EnsureWorkspaceBase
            | Self::BuildWorkspaceBase
            | Self::CommitToWorkspace
            | Self::CommitToGit
            | Self::WorkspaceBinding
            | Self::AuditPull
            | Self::AuditSnapshot
            | Self::AuditResetFloor
            | Self::ReadFile
            | Self::WriteFile
            | Self::EditFile
            | Self::PluginEnsure
            | Self::PluginStatus
            | Self::IsolatedWorkspaceEnter
            | Self::IsolatedWorkspaceExit
            | Self::IsolatedWorkspaceStatus
            | Self::IsolatedWorkspaceListOpen
            | Self::ExecCommand
            | Self::WriteStdin
            | Self::CommandReadProgress
            | Self::CommandCancel
            | Self::CommandCollectCompleted
            | Self::CommandSessionCount
            | Self::CancelWorkspaceRunsByCaller
            | Self::CancelWorkspaceRuns => false,
        }
    }

    /// Build the protocol catalog entry for this op.
    #[must_use]
    pub const fn spec(self) -> BuiltinOpSpec {
        BuiltinOpSpec {
            op: self,
            wire: self.wire(),
            family: self.family(),
            mutates_state: self.mutates_state(),
            test_only: self.test_only(),
        }
    }
}

/// Protocol metadata for one built-in daemon op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinOpSpec {
    /// Typed op identity.
    pub op: BuiltinDaemonOp,
    /// Verbatim wire string.
    pub wire: &'static str,
    /// Functional owner.
    pub family: OpFamily,
    /// Whether the op may change daemon, workspace, or process state.
    pub mutates_state: bool,
    /// Whether the op is a daemon-side test hook.
    pub test_only: bool,
}

/// Runtime readiness probe.
pub const API_RUNTIME_READY: &str = BuiltinDaemonOp::RuntimeReady.wire();
/// Invocation heartbeat.
pub const API_V1_HEARTBEAT: &str = BuiltinDaemonOp::InvocationHeartbeat.wire();
/// Cancel an in-flight invocation.
pub const API_V1_CANCEL: &str = BuiltinDaemonOp::InvocationCancel.wire();
/// Count in-flight invocations.
pub const API_V1_INFLIGHT_COUNT: &str = BuiltinDaemonOp::InflightCount.wire();
/// LayerStack/storage metrics.
pub const API_LAYER_METRICS: &str = BuiltinDaemonOp::LayerMetrics.wire();
/// Ensure a workspace base binding exists.
pub const API_ENSURE_WORKSPACE_BASE: &str = BuiltinDaemonOp::EnsureWorkspaceBase.wire();
/// Build or rebuild a workspace base binding.
pub const API_BUILD_WORKSPACE_BASE: &str = BuiltinDaemonOp::BuildWorkspaceBase.wire();
/// Materialize LayerStack state into the bound workspace.
pub const API_COMMIT_TO_WORKSPACE: &str = BuiltinDaemonOp::CommitToWorkspace.wire();
/// Commit a LayerStack snapshot into the bound workspace's durable Git repo.
pub const API_COMMIT_TO_GIT: &str = BuiltinDaemonOp::CommitToGit.wire();
/// Inspect the workspace binding for a layer stack root.
pub const API_WORKSPACE_BINDING: &str = BuiltinDaemonOp::WorkspaceBinding.wire();
/// Pull audit events after a cursor.
pub const API_AUDIT_PULL: &str = BuiltinDaemonOp::AuditPull.wire();
/// Snapshot audit ring metadata.
pub const API_AUDIT_SNAPSHOT: &str = BuiltinDaemonOp::AuditSnapshot.wire();
/// Reset the audit floor when daemon-side test gate allows it.
pub const API_AUDIT_RESET_FLOOR: &str = BuiltinDaemonOp::AuditResetFloor.wire();
/// Direct LayerStack read.
pub const API_V1_READ_FILE: &str = BuiltinDaemonOp::ReadFile.wire();
/// Direct OCC-gated write.
pub const API_V1_WRITE_FILE: &str = BuiltinDaemonOp::WriteFile.wire();
/// Direct OCC-gated edit.
pub const API_V1_EDIT_FILE: &str = BuiltinDaemonOp::EditFile.wire();
/// Ensure a plugin service is available.
pub const API_PLUGIN_ENSURE: &str = BuiltinDaemonOp::PluginEnsure.wire();
/// Inspect plugin service status.
pub const API_PLUGIN_STATUS: &str = BuiltinDaemonOp::PluginStatus.wire();
/// Enter isolated workspace mode.
pub const API_ISOLATED_WORKSPACE_ENTER: &str = BuiltinDaemonOp::IsolatedWorkspaceEnter.wire();
/// Exit isolated workspace mode.
pub const API_ISOLATED_WORKSPACE_EXIT: &str = BuiltinDaemonOp::IsolatedWorkspaceExit.wire();
/// Inspect isolated workspace status.
pub const API_ISOLATED_WORKSPACE_STATUS: &str = BuiltinDaemonOp::IsolatedWorkspaceStatus.wire();
/// List open isolated workspaces.
pub const API_ISOLATED_WORKSPACE_LIST_OPEN: &str =
    BuiltinDaemonOp::IsolatedWorkspaceListOpen.wire();
/// Test-only isolated workspace reset hook.
pub const API_ISOLATED_WORKSPACE_TEST_RESET: &str =
    BuiltinDaemonOp::IsolatedWorkspaceTestReset.wire();
/// Start or poll a command session.
pub const API_V1_EXEC_COMMAND: &str = BuiltinDaemonOp::ExecCommand.wire();
/// Write stdin to a command session.
pub const API_V1_WRITE_STDIN: &str = BuiltinDaemonOp::WriteStdin.wire();
/// Read command-session progress without writing stdin.
pub const API_V1_COMMAND_READ_PROGRESS: &str = BuiltinDaemonOp::CommandReadProgress.wire();
/// Cancel a command session.
pub const API_V1_COMMAND_CANCEL: &str = BuiltinDaemonOp::CommandCancel.wire();
/// Collect completed command-session notifications.
pub const API_V1_COMMAND_COLLECT_COMPLETED: &str = BuiltinDaemonOp::CommandCollectCompleted.wire();
/// Count live command sessions.
pub const API_V1_COMMAND_SESSION_COUNT: &str = BuiltinDaemonOp::CommandSessionCount.wire();
/// Cancel every workspace run owned by one caller (`caller_id == agent_run_id`):
/// discards the caller's command session(s) and exits its isolated workspace if
/// open. The agent-core per-run cancellation RPC.
pub const API_V1_CANCEL_WORKSPACE_RUNS_BY_CALLER: &str =
    BuiltinDaemonOp::CancelWorkspaceRunsByCaller.wire();
/// Cancel every workspace run in the sandbox (whole-sandbox sweep backstop):
/// discards all command sessions, exits all isolated callers, reaps orphans.
pub const API_V1_CANCEL_WORKSPACE_RUNS: &str = BuiltinDaemonOp::CancelWorkspaceRuns.wire();

/// Built-in daemon op metadata expected to be available over the wire.
pub const BUILTIN_DAEMON_OP_SPECS: &[BuiltinOpSpec] = &[
    BuiltinDaemonOp::RuntimeReady.spec(),
    BuiltinDaemonOp::InvocationHeartbeat.spec(),
    BuiltinDaemonOp::InvocationCancel.spec(),
    BuiltinDaemonOp::InflightCount.spec(),
    BuiltinDaemonOp::LayerMetrics.spec(),
    BuiltinDaemonOp::EnsureWorkspaceBase.spec(),
    BuiltinDaemonOp::BuildWorkspaceBase.spec(),
    BuiltinDaemonOp::CommitToWorkspace.spec(),
    BuiltinDaemonOp::CommitToGit.spec(),
    BuiltinDaemonOp::WorkspaceBinding.spec(),
    BuiltinDaemonOp::AuditPull.spec(),
    BuiltinDaemonOp::AuditSnapshot.spec(),
    BuiltinDaemonOp::AuditResetFloor.spec(),
    BuiltinDaemonOp::ReadFile.spec(),
    BuiltinDaemonOp::WriteFile.spec(),
    BuiltinDaemonOp::EditFile.spec(),
    BuiltinDaemonOp::PluginEnsure.spec(),
    BuiltinDaemonOp::PluginStatus.spec(),
    BuiltinDaemonOp::IsolatedWorkspaceEnter.spec(),
    BuiltinDaemonOp::IsolatedWorkspaceExit.spec(),
    BuiltinDaemonOp::IsolatedWorkspaceStatus.spec(),
    BuiltinDaemonOp::IsolatedWorkspaceListOpen.spec(),
    BuiltinDaemonOp::IsolatedWorkspaceTestReset.spec(),
    BuiltinDaemonOp::ExecCommand.spec(),
    BuiltinDaemonOp::WriteStdin.spec(),
    BuiltinDaemonOp::CommandReadProgress.spec(),
    BuiltinDaemonOp::CommandCancel.spec(),
    BuiltinDaemonOp::CommandCollectCompleted.spec(),
    BuiltinDaemonOp::CommandSessionCount.spec(),
    BuiltinDaemonOp::CancelWorkspaceRunsByCaller.spec(),
    BuiltinDaemonOp::CancelWorkspaceRuns.spec(),
];

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn builtin_specs_match_wire_list() {
        let catalog_wires = BUILTIN_DAEMON_OP_SPECS
            .iter()
            .map(|spec| spec.wire)
            .collect::<Vec<_>>();
        assert_eq!(catalog_wires, BUILTIN_DAEMON_OPS);
    }

    #[test]
    fn builtin_specs_have_no_duplicate_ops_or_wires() {
        let unique_ops = BUILTIN_DAEMON_OP_SPECS
            .iter()
            .map(|spec| spec.op)
            .collect::<BTreeSet<_>>();
        let unique_wires = BUILTIN_DAEMON_OP_SPECS
            .iter()
            .map(|spec| spec.wire)
            .collect::<BTreeSet<_>>();
        assert_eq!(unique_ops.len(), BUILTIN_DAEMON_OP_SPECS.len());
        assert_eq!(unique_wires.len(), BUILTIN_DAEMON_OP_SPECS.len());
    }

    #[test]
    fn builtin_spec_metadata_matches_op_methods() {
        for spec in BUILTIN_DAEMON_OP_SPECS {
            assert_eq!(spec.wire, spec.op.wire());
            assert_eq!(spec.family, spec.op.family());
            assert_eq!(spec.mutates_state, spec.op.mutates_state());
            assert_eq!(spec.test_only, spec.op.test_only());
        }
    }
}
