use async_trait::async_trait;
use eos_sandbox_port::{
    CommandSessionCancelRequest, ExecCommandRequest, ExecCommandResult, ExecStdinRequest,
    ReadCommandProgressRequest,
};
use eos_types::{AgentRunId, CommandSessionId, JsonObject, SandboxId};

use crate::{Sealed, ToolError};

/// Resource service for sandbox command operations.
#[async_trait]
pub trait CommandServicePort: Sealed + Send + Sync {
    /// Run or start a managed command session.
    async fn exec_command(
        &self,
        sandbox_id: &SandboxId,
        request: &ExecCommandRequest,
    ) -> Result<ExecCommandResult, ToolError>;

    /// Write stdin to an open command session.
    async fn write_stdin(
        &self,
        sandbox_id: &SandboxId,
        request: &ExecStdinRequest,
    ) -> Result<ExecCommandResult, ToolError>;

    /// Read command progress.
    async fn read_command_progress(
        &self,
        sandbox_id: &SandboxId,
        request: &ReadCommandProgressRequest,
    ) -> Result<ExecCommandResult, ToolError>;

    /// Cancel one command session.
    async fn cancel_command_session(
        &self,
        sandbox_id: &SandboxId,
        request: &CommandSessionCancelRequest,
    ) -> Result<ExecCommandResult, ToolError>;

    /// Collect completed background command sessions for one caller.
    async fn collect_completed_commands(
        &self,
        sandbox_id: &SandboxId,
        caller_id: &AgentRunId,
        command_session_ids: &[CommandSessionId],
    ) -> Result<Vec<JsonObject>, ToolError>;

    /// Cancel every command/workspace run owned by one caller.
    async fn cancel_commands_for_run(
        &self,
        sandbox_id: &SandboxId,
        caller_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), ToolError>;
}

/// Command background-session registry for one owning agent run.
#[async_trait]
pub trait CommandSessionPort: Sealed + Send + Sync {
    /// Register a freshly-started background command session as running.
    async fn register_background_session(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
    );

    /// Count running command sessions for this run.
    async fn count_background_sessions(&self) -> usize;

    /// Cancel all running command sessions for this run.
    async fn cancel_all_background_sessions(&self, reason: &str);

    /// Poll terminal command sessions and push notifications.
    async fn poll_complete_background_sessions(&self) -> usize;
}
