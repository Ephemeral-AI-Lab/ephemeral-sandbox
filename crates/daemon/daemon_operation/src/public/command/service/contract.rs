use crate::workspace_crate::WorkspaceSessionId;

pub use command::{CommandStream, CommandTranscriptRow};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandSessionId(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct ExecCommandInput {
    pub workspace_session_id: WorkspaceSessionId,
    pub cmd: String,
    pub timeout_seconds: Option<f64>,
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
    pub start_offset: u64,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollCommandInput {
    pub command_session_id: CommandSessionId,
    pub last_n_lines: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelCommandInput {
    pub command_session_id: CommandSessionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutputSnapshot {
    pub stdout: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandFinalizedMetadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandYield {
    pub command_session_id: Option<CommandSessionId>,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub output: CommandOutputSnapshot,
    pub finalized: Option<CommandFinalizedMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPollOutput {
    pub command_session_id: CommandSessionId,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub output: CommandOutputSnapshot,
    pub finalized: Option<CommandFinalizedMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLinesOutput {
    pub command_session_id: CommandSessionId,
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub start_offset: u64,
    pub end_offset: u64,
    pub total_lines: u64,
    pub truncated_before: u64,
    pub output_truncated: bool,
    pub output: Vec<CommandTranscriptRow>,
}
