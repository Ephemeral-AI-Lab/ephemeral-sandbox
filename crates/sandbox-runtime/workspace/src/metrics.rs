use std::sync::Arc;
use std::time::Duration;

use crate::namespace::cgroup_monitor::{CgroupMonitorSample, CgroupMonitorTargetKind};

pub type RuntimeMetricsRecorderHandle = Arc<dyn RuntimeMetricsRecorder>;

pub trait RuntimeMetricsRecorder: Send + Sync {
    fn record_runtime_latency(
        &self,
        _operation: RuntimeOperationName,
        _status: RuntimeMetricStatus,
        _latency: Duration,
    ) {
    }

    fn record_workspace_phase(
        &self,
        _phase: WorkspacePhase,
        _status: RuntimeMetricStatus,
        _latency: Duration,
    ) {
    }

    fn record_cgroup_sample(
        &self,
        _target_kind: CgroupMonitorTargetKind,
        _sample: &CgroupMonitorSample,
    ) {
    }

    fn record_publish_rejection(&self, _reason: PublishRejectionReason) {}

    fn record_remount_failure(&self, _reason: RemountFailureReason) {}

    fn record_command_cancellation(&self, _reason: CommandCancellationReason) {}

    fn record_cgroup_read_error(
        &self,
        _target_kind: CgroupMonitorTargetKind,
        _error_kind: CgroupReadErrorKind,
    ) {
    }
}

#[derive(Debug, Default)]
pub struct NoopRuntimeMetricsRecorder;

impl RuntimeMetricsRecorder for NoopRuntimeMetricsRecorder {}

#[must_use]
pub fn noop_runtime_metrics_recorder() -> RuntimeMetricsRecorderHandle {
    Arc::new(NoopRuntimeMetricsRecorder)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeOperationName {
    ExecCommand,
    WriteCommandStdin,
    ReadCommandLines,
}

impl RuntimeOperationName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExecCommand => "exec_command",
            Self::WriteCommandStdin => "write_command_stdin",
            Self::ReadCommandLines => "read_command_lines",
        }
    }

    #[must_use]
    pub fn from_static_name(name: &str) -> Option<Self> {
        match name {
            "exec_command" => Some(Self::ExecCommand),
            "write_command_stdin" => Some(Self::WriteCommandStdin),
            "read_command_lines" => Some(Self::ReadCommandLines),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspacePhase {
    CreateSession,
    DestroySession,
    PublishChanges,
    RemountWorkspace,
}

impl WorkspacePhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateSession => "create_session",
            Self::DestroySession => "destroy_session",
            Self::PublishChanges => "publish_changes",
            Self::RemountWorkspace => "remount_workspace",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeMetricStatus {
    Ok,
    Running,
    Error,
    Cancelled,
    TimedOut,
    Rejected,
    Blocked,
}

impl RuntimeMetricStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Running => "running",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Rejected => "rejected",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PublishRejectionReason {
    InvalidBaseRevision,
    GitMutationForbidden,
    ProtectedPath,
    SourceConflict,
    OpaqueDirProtectedDescendant,
    OpaqueDirMixedRoutes,
    OpaqueDirExpansionLimit,
    RoutePreparationFailed,
    LayerStackConflict,
}

impl PublishRejectionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidBaseRevision => "invalid_base_revision",
            Self::GitMutationForbidden => "git_mutation_forbidden",
            Self::ProtectedPath => "protected_path",
            Self::SourceConflict => "source_conflict",
            Self::OpaqueDirProtectedDescendant => "opaque_dir_protected_descendant",
            Self::OpaqueDirMixedRoutes => "opaque_dir_mixed_routes",
            Self::OpaqueDirExpansionLimit => "opaque_dir_expansion_limit",
            Self::RoutePreparationFailed => "route_preparation_failed",
            Self::LayerStackConflict => "layerstack_conflict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemountFailureReason {
    ActiveCommandMissing,
    ProcessGroupUnavailable,
    RemountCancelledBeforeSwitch,
    ProcessGroupBlocked,
    WorkspaceSession,
    Command,
}

impl RemountFailureReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActiveCommandMissing => "active_command_missing",
            Self::ProcessGroupUnavailable => "process_group_unavailable",
            Self::RemountCancelledBeforeSwitch => "remount_cancelled_before_switch",
            Self::ProcessGroupBlocked => "process_group_blocked",
            Self::WorkspaceSession => "workspace_session",
            Self::Command => "command",
        }
    }

    #[must_use]
    pub fn from_block_reason(reason: &str) -> Self {
        match reason {
            "active_command_missing" => Self::ActiveCommandMissing,
            "process_group_unavailable" => Self::ProcessGroupUnavailable,
            "remount_cancelled_before_switch" => Self::RemountCancelledBeforeSwitch,
            _ => Self::ProcessGroupBlocked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCancellationReason {
    StdinSignal,
    RemountCancellation,
    StartupRollback,
}

impl CommandCancellationReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StdinSignal => "stdin_signal",
            Self::RemountCancellation => "remount_cancellation",
            Self::StartupRollback => "startup_rollback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CgroupReadErrorKind {
    CgroupMissing,
    MissingCgroupFile,
    MalformedCgroupFile,
    ReadError,
}

impl CgroupReadErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CgroupMissing => "cgroup_missing",
            Self::MissingCgroupFile => "missing_cgroup_file",
            Self::MalformedCgroupFile => "malformed_cgroup_file",
            Self::ReadError => "read_error",
        }
    }

    #[must_use]
    pub fn from_sample(sample: &CgroupMonitorSample) -> Option<Self> {
        let error = sample.state.read_error.as_deref()?;
        if !sample.state.cgroup_exists {
            Some(Self::CgroupMissing)
        } else if error.contains("malformed") {
            Some(Self::MalformedCgroupFile)
        } else if error.contains("No such file") || error.contains("not found") {
            Some(Self::MissingCgroupFile)
        } else {
            Some(Self::ReadError)
        }
    }
}
