use std::path::PathBuf;

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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandOutputSnapshot {
    pub start_offset: u64,
    pub end_offset: u64,
    pub total_lines: u64,
    pub original_token_count: u64,
    pub output: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandFinalizedMetadata {
    pub publish: Option<CommandPublishFinalization>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPublishFinalization {
    pub status: CommandPublishStatus,
    pub rejection: Option<Box<sandbox_runtime_layerstack::PublishReject>>,
    pub revision: Option<crate::layerstack::LayerStackRevision>,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPublishStatus {
    Published,
    NoOp,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandYield {
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

#[derive(Debug, Clone, PartialEq)]
pub struct CommandLinesOutput {
    pub command_session_id: CommandSessionId,
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
