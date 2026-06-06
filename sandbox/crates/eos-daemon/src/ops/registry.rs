//! Built-in daemon operation registry.

use eos_protocol::ops as protocol_ops;

use crate::dispatcher::Handler;

use super::{audit, checkpoint, command_sessions, control, files, isolated_workspace, plugins};

#[derive(Clone, Copy)]
pub(crate) struct BuiltinOp {
    pub(crate) wire: &'static str,
    pub(crate) handler: Handler,
}

pub(crate) const BUILTIN_OPS: &[BuiltinOp] = &[
    BuiltinOp {
        wire: protocol_ops::API_RUNTIME_READY,
        handler: control::op_runtime_ready,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_HEARTBEAT,
        handler: control::op_heartbeat,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_CANCEL,
        handler: control::op_cancel,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_INFLIGHT_COUNT,
        handler: control::op_inflight_count,
    },
    BuiltinOp {
        wire: protocol_ops::API_LAYER_METRICS,
        handler: checkpoint::op_layer_metrics,
    },
    BuiltinOp {
        wire: protocol_ops::API_ENSURE_WORKSPACE_BASE,
        handler: checkpoint::op_ensure_workspace_base,
    },
    BuiltinOp {
        wire: protocol_ops::API_BUILD_WORKSPACE_BASE,
        handler: checkpoint::op_build_workspace_base,
    },
    BuiltinOp {
        wire: protocol_ops::API_COMMIT_TO_WORKSPACE,
        handler: checkpoint::op_commit_to_workspace,
    },
    BuiltinOp {
        wire: protocol_ops::API_COMMIT_TO_GIT,
        handler: checkpoint::op_commit_to_git,
    },
    BuiltinOp {
        wire: protocol_ops::API_WORKSPACE_BINDING,
        handler: checkpoint::op_workspace_binding,
    },
    BuiltinOp {
        wire: protocol_ops::API_AUDIT_PULL,
        handler: audit::op_audit_pull,
    },
    BuiltinOp {
        wire: protocol_ops::API_AUDIT_SNAPSHOT,
        handler: audit::op_audit_snapshot,
    },
    BuiltinOp {
        wire: protocol_ops::API_AUDIT_RESET_FLOOR,
        handler: audit::op_audit_reset_floor,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_READ_FILE,
        handler: files::op_read_file,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_WRITE_FILE,
        handler: files::op_write_file,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_EDIT_FILE,
        handler: files::op_edit_file,
    },
    BuiltinOp {
        wire: protocol_ops::API_PLUGIN_ENSURE,
        handler: plugins::op_ensure,
    },
    BuiltinOp {
        wire: protocol_ops::API_PLUGIN_STATUS,
        handler: plugins::op_status,
    },
    BuiltinOp {
        wire: protocol_ops::API_ISOLATED_WORKSPACE_ENTER,
        handler: isolated_workspace::op_enter,
    },
    BuiltinOp {
        wire: protocol_ops::API_ISOLATED_WORKSPACE_EXIT,
        handler: isolated_workspace::op_exit,
    },
    BuiltinOp {
        wire: protocol_ops::API_ISOLATED_WORKSPACE_STATUS,
        handler: isolated_workspace::op_status,
    },
    BuiltinOp {
        wire: protocol_ops::API_ISOLATED_WORKSPACE_LIST_OPEN,
        handler: isolated_workspace::op_list_open,
    },
    BuiltinOp {
        wire: protocol_ops::API_ISOLATED_WORKSPACE_TEST_RESET,
        handler: isolated_workspace::op_test_reset,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_EXEC_COMMAND,
        handler: command_sessions::op_exec_command,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_WRITE_STDIN,
        handler: command_sessions::op_command_write_stdin,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_COMMAND_CANCEL,
        handler: command_sessions::op_command_cancel,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_COMMAND_COLLECT_COMPLETED,
        handler: command_sessions::op_command_collect_completed,
    },
    BuiltinOp {
        wire: protocol_ops::API_V1_COMMAND_SESSION_COUNT,
        handler: command_sessions::op_command_session_count,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use eos_protocol::ops::BUILTIN_DAEMON_OPS;

    use super::*;

    #[test]
    fn builtin_registry_matches_protocol_ops() {
        let registered = BUILTIN_OPS.iter().map(|op| op.wire).collect::<Vec<_>>();
        assert_eq!(registered, BUILTIN_DAEMON_OPS);
    }

    #[test]
    fn builtin_registry_has_no_duplicate_wires() {
        let registered = BUILTIN_OPS.iter().map(|op| op.wire).collect::<Vec<_>>();
        let unique = registered.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), registered.len());
    }
}
