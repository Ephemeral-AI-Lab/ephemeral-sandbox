use crate::workspace_crate::WorkspaceSessionId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandSessionId(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct ExecCommandInput {
    pub workspace_session_id: Option<WorkspaceSessionId>,
    pub cmd: String,
    pub timeout_ms: Option<u64>,
    pub yield_time_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteCommandStdinInput {
    pub command_session_id: CommandSessionId,
    pub stdin: String,
    pub yield_time_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCommandLinesInput {
    pub command_session_id: CommandSessionId,
    pub start_offset: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Running,
    Ok,
    Error,
    TimedOut,
    Cancelled,
}

impl CommandStatus {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The single command output DTO: the merge of the former `CommandYield`,
/// `CommandLinesOutput`, and `CommandOutputSnapshot`. `command_session_id` is
/// `Option` (the superset): yields include it only when the command is still
/// running or has more output to drain; `read_command_lines` always sets it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    pub command_session_id: Option<CommandSessionId>,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub wall_time_seconds: f64,
    pub command_total_time_seconds: f64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub total_lines: u64,
    pub original_token_count: u64,
    pub output: String,
}
