//! Typed daemon operation constants.
//!
//! Replaces the bare `DAEMON_OP_*` `&str` constants from
//! `sandbox/api/transport.py` with one `#[non_exhaustive]` enum whose serialized
//! form is the **exact** legacy wire string (`api.v1.read_file`, …) so the
//! protocol stays byte-compatible (GC-sandbox-api-02). New ops are added as
//! variants, never by editing a stringly dispatch (`type-no-stringly`, OCP).

use serde::{Deserialize, Serialize};

/// One sandbox daemon operation. Serializes to its verbatim legacy wire string.
///
/// `#[non_exhaustive]` because the daemon protocol may grow; within this crate
/// matches stay exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DaemonOp {
    /// `api.v1.read_file`
    #[serde(rename = "api.v1.read_file")]
    ReadFile,
    /// `api.v1.write_file`
    #[serde(rename = "api.v1.write_file")]
    WriteFile,
    /// `api.v1.edit_file`
    #[serde(rename = "api.v1.edit_file")]
    EditFile,
    /// `api.v1.exec_command`
    #[serde(rename = "api.v1.exec_command")]
    ExecCommand,
    /// `api.v1.exec_stdin`
    #[serde(rename = "api.v1.exec_stdin")]
    ExecStdin,
    /// `api.v1.command.cancel`
    #[serde(rename = "api.v1.command.cancel")]
    CommandCancel,
    /// `api.v1.command.collect_completed`
    #[serde(rename = "api.v1.command.collect_completed")]
    CommandCollectCompleted,
    /// `api.v1.command_session_count`
    #[serde(rename = "api.v1.command_session_count")]
    CommandSessionCount,
    /// `api.v1.cancel`
    #[serde(rename = "api.v1.cancel")]
    InvocationCancel,
    /// `api.v1.heartbeat`
    #[serde(rename = "api.v1.heartbeat")]
    InvocationHeartbeat,
    /// `api.v1.inflight_count`
    #[serde(rename = "api.v1.inflight_count")]
    InflightCount,
    /// `api.isolated_workspace.enter`
    #[serde(rename = "api.isolated_workspace.enter")]
    IsolatedWorkspaceEnter,
    /// `api.isolated_workspace.exit`
    #[serde(rename = "api.isolated_workspace.exit")]
    IsolatedWorkspaceExit,
    /// `api.isolated_workspace.status`
    #[serde(rename = "api.isolated_workspace.status")]
    IsolatedWorkspaceStatus,
    /// `api.v1.glob`
    #[serde(rename = "api.v1.glob")]
    Glob,
    /// `api.v1.grep`
    #[serde(rename = "api.v1.grep")]
    Grep,
    /// `api.audit.pull`
    #[serde(rename = "api.audit.pull")]
    AuditPull,
    /// `api.audit.snapshot`
    #[serde(rename = "api.audit.snapshot")]
    AuditSnapshot,
    /// `api.audit.reset_floor`
    #[serde(rename = "api.audit.reset_floor")]
    AuditResetFloor,
}

impl DaemonOp {
    /// The verbatim legacy wire string for this op. Mirrors the serde encoding;
    /// the test `daemon_op_wire_strings` pins both to the same value so they
    /// cannot drift.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::ReadFile => "api.v1.read_file",
            Self::WriteFile => "api.v1.write_file",
            Self::EditFile => "api.v1.edit_file",
            Self::ExecCommand => "api.v1.exec_command",
            Self::ExecStdin => "api.v1.exec_stdin",
            Self::CommandCancel => "api.v1.command.cancel",
            Self::CommandCollectCompleted => "api.v1.command.collect_completed",
            Self::CommandSessionCount => "api.v1.command_session_count",
            Self::InvocationCancel => "api.v1.cancel",
            Self::InvocationHeartbeat => "api.v1.heartbeat",
            Self::InflightCount => "api.v1.inflight_count",
            Self::IsolatedWorkspaceEnter => "api.isolated_workspace.enter",
            Self::IsolatedWorkspaceExit => "api.isolated_workspace.exit",
            Self::IsolatedWorkspaceStatus => "api.isolated_workspace.status",
            Self::Glob => "api.v1.glob",
            Self::Grep => "api.v1.grep",
            Self::AuditPull => "api.audit.pull",
            Self::AuditSnapshot => "api.audit.snapshot",
            Self::AuditResetFloor => "api.audit.reset_floor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-sandbox-api-06: every variant serializes to the exact legacy wire
    // string from transport.py, and `as_wire` agrees with serde (no drift).
    #[test]
    fn daemon_op_wire_strings() {
        let table = [
            (DaemonOp::ReadFile, "api.v1.read_file"),
            (DaemonOp::WriteFile, "api.v1.write_file"),
            (DaemonOp::EditFile, "api.v1.edit_file"),
            (DaemonOp::ExecCommand, "api.v1.exec_command"),
            (DaemonOp::ExecStdin, "api.v1.exec_stdin"),
            (DaemonOp::CommandCancel, "api.v1.command.cancel"),
            (
                DaemonOp::CommandCollectCompleted,
                "api.v1.command.collect_completed",
            ),
            (
                DaemonOp::CommandSessionCount,
                "api.v1.command_session_count",
            ),
            (DaemonOp::InvocationCancel, "api.v1.cancel"),
            (DaemonOp::InvocationHeartbeat, "api.v1.heartbeat"),
            (DaemonOp::InflightCount, "api.v1.inflight_count"),
            (
                DaemonOp::IsolatedWorkspaceEnter,
                "api.isolated_workspace.enter",
            ),
            (
                DaemonOp::IsolatedWorkspaceExit,
                "api.isolated_workspace.exit",
            ),
            (
                DaemonOp::IsolatedWorkspaceStatus,
                "api.isolated_workspace.status",
            ),
            (DaemonOp::Glob, "api.v1.glob"),
            (DaemonOp::Grep, "api.v1.grep"),
            (DaemonOp::AuditPull, "api.audit.pull"),
            (DaemonOp::AuditSnapshot, "api.audit.snapshot"),
            (DaemonOp::AuditResetFloor, "api.audit.reset_floor"),
        ];
        for (op, wire) in table {
            assert_eq!(op.as_wire(), wire, "as_wire for {op:?}");
            assert_eq!(
                serde_json::to_value(op).expect("serialize op"),
                serde_json::Value::String(wire.to_owned()),
                "serde for {op:?}"
            );
            let back: DaemonOp = serde_json::from_value(serde_json::Value::String(wire.to_owned()))
                .expect("deserialize op");
            assert_eq!(back, op, "roundtrip for {op:?}");
        }
    }
}
