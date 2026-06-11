//! Built-in daemon operation registry.

use crate::wire::ops as protocol_ops;

use crate::dispatcher::Handler;
use crate::ops::cancel;
use crate::ops::checkpoint as checkpoint_ops;
use crate::ops::command as run_ops;
use crate::ops::control;
use crate::ops::files as file_ops;
use crate::ops::isolation as isolated;
use crate::ops::plugin as plugins;

#[derive(Clone, Copy)]
pub(crate) struct BuiltinOp {
    pub(crate) spec: protocol_ops::BuiltinOpSpec,
    pub(crate) handler: Handler,
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
        handler: checkpoint_ops::op_layer_metrics,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::EnsureWorkspaceBase.spec(),
        handler: checkpoint_ops::op_ensure_workspace_base,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::BuildWorkspaceBase.spec(),
        handler: checkpoint_ops::op_build_workspace_base,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommitToWorkspace.spec(),
        handler: checkpoint_ops::op_commit_to_workspace,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommitToGit.spec(),
        handler: checkpoint_ops::op_commit_to_git,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WorkspaceBinding.spec(),
        handler: checkpoint_ops::op_workspace_binding,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::ReadFile.spec(),
        handler: file_ops::op_read_file,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WriteFile.spec(),
        handler: file_ops::op_write_file,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::EditFile.spec(),
        handler: file_ops::op_edit_file,
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
        handler: isolated::op_enter,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceExit.spec(),
        handler: isolated::op_exit,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceStatus.spec(),
        handler: isolated::op_status,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceListOpen.spec(),
        handler: isolated::op_list_open,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::IsolatedWorkspaceTestReset.spec(),
        handler: isolated::op_test_reset,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::ExecCommand.spec(),
        handler: run_ops::op_exec_command,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::WriteStdin.spec(),
        handler: run_ops::op_command_write_stdin,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandReadProgress.spec(),
        handler: run_ops::op_command_read_progress,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandCancel.spec(),
        handler: run_ops::op_command_cancel,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandCollectCompleted.spec(),
        handler: run_ops::op_command_collect_completed,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CommandSessionCount.spec(),
        handler: run_ops::op_command_session_count,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CancelWorkspaceRunsByCaller.spec(),
        handler: cancel::op_cancel_workspace_runs_by_caller_id,
    },
    BuiltinOp {
        spec: protocol_ops::BuiltinDaemonOp::CancelWorkspaceRuns.spec(),
        handler: cancel::op_cancel_workspace_runs,
    },
];

#[cfg(test)]
#[path = "../../tests/unit/ops_registry/mod.rs"]
mod tests;
