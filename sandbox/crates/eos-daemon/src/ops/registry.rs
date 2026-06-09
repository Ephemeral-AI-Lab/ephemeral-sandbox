//! Built-in daemon operation registry.

use eos_protocol::ops as protocol_ops;

use crate::dispatcher::Handler;

use super::{
    audit, checkpoint, command_sessions, control, files, isolated_workspace, plugins, workspace_run,
};

#[derive(Clone, Copy)]
pub(crate) struct BuiltinOp {
    pub(crate) spec: protocol_ops::BuiltinOpSpec,
    pub(crate) handler: Handler,
}

impl BuiltinOp {
    pub(crate) const fn wire(&self) -> &'static str {
        self.spec.wire
    }
}

pub(crate) const BUILTIN_OPS: &[BuiltinOp] = &[
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::RuntimeReady.spec(),
        handler: control::op_runtime_ready,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::InvocationHeartbeat.spec(),
        handler: control::op_heartbeat,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::InvocationCancel.spec(),
        handler: control::op_cancel,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::InflightCount.spec(),
        handler: control::op_inflight_count,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::LayerMetrics.spec(),
        handler: checkpoint::op_layer_metrics,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::EnsureWorkspaceBase.spec(),
        handler: checkpoint::op_ensure_workspace_base,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::BuildWorkspaceBase.spec(),
        handler: checkpoint::op_build_workspace_base,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommitToWorkspace.spec(),
        handler: checkpoint::op_commit_to_workspace,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommitToGit.spec(),
        handler: checkpoint::op_commit_to_git,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WorkspaceBinding.spec(),
        handler: checkpoint::op_workspace_binding,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::AuditPull.spec(),
        handler: audit::op_audit_pull,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::AuditSnapshot.spec(),
        handler: audit::op_audit_snapshot,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::AuditResetFloor.spec(),
        handler: audit::op_audit_reset_floor,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::ReadFile.spec(),
        handler: files::op_read_file,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WriteFile.spec(),
        handler: files::op_write_file,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::EditFile.spec(),
        handler: files::op_edit_file,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::PluginEnsure.spec(),
        handler: plugins::op_ensure,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::PluginStatus.spec(),
        handler: plugins::op_status,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceEnter.spec(),
        handler: isolated_workspace::op_enter,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceExit.spec(),
        handler: isolated_workspace::op_exit,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceStatus.spec(),
        handler: isolated_workspace::op_status,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceListOpen.spec(),
        handler: isolated_workspace::op_list_open,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceTestReset.spec(),
        handler: isolated_workspace::op_test_reset,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::ExecCommand.spec(),
        handler: command_sessions::op_exec_command,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WriteStdin.spec(),
        handler: command_sessions::op_command_write_stdin,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandReadProgress.spec(),
        handler: command_sessions::op_command_read_progress,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandCancel.spec(),
        handler: command_sessions::op_command_cancel,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandCollectCompleted.spec(),
        handler: command_sessions::op_command_collect_completed,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandSessionCount.spec(),
        handler: command_sessions::op_command_session_count,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CancelWorkspaceRunsByCaller.spec(),
        handler: workspace_run::op_cancel_workspace_runs_by_caller_id,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CancelWorkspaceRuns.spec(),
        handler: workspace_run::op_cancel_workspace_runs,
    },
];

#[cfg(test)]
#[path = "../../tests/ops_registry/mod.rs"]
mod tests;
