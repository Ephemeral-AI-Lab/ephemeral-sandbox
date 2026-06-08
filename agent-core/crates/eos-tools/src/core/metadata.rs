//! Service-free execution facts supplied to one tool call.

use std::sync::Arc;

use eos_llm_client::Message;
use eos_types::{
    AgentRunId, AttemptId, InvocationId, RequestId, SandboxId, TaskId, ToolUseId, WorkflowId,
};

use crate::ToolError;

/// The typed facts a tool executor reads. Built per tool call and owned by the
/// call; no shared mutable service state is stored here.
#[derive(Clone)]
pub struct ExecutionMetadata {
    /// Bound agent profile name.
    pub agent_name: String,
    /// Agent-run id.
    pub agent_run_id: Option<AgentRunId>,
    /// Owning request, when set.
    pub request_id: Option<RequestId>,
    /// Owning task, when set.
    pub task_id: Option<TaskId>,
    /// Owning attempt, when set.
    pub attempt_id: Option<AttemptId>,
    /// Owning workflow, when set.
    pub workflow_id: Option<WorkflowId>,
    /// Per-call tool-use id.
    pub tool_use_id: Option<ToolUseId>,
    /// In-flight sandbox correlation id, when set.
    pub sandbox_invocation_id: Option<InvocationId>,
    /// Provisioned sandbox, when the agent is sandbox-bound.
    pub sandbox_id: Option<SandboxId>,
    /// Whether this agent currently has an open isolated workspace.
    pub is_isolated_workspace_mode: bool,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Per-turn snapshot of the live conversation transcript.
    pub conversation: Arc<[Message]>,
}

impl std::fmt::Debug for ExecutionMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionMetadata")
            .field("agent_name", &self.agent_name)
            .field("agent_run_id", &self.agent_run_id)
            .field("request_id", &self.request_id)
            .field("task_id", &self.task_id)
            .field("attempt_id", &self.attempt_id)
            .field("workflow_id", &self.workflow_id)
            .field("tool_use_id", &self.tool_use_id)
            .field("sandbox_id", &self.sandbox_id)
            .field(
                "is_isolated_workspace_mode",
                &self.is_isolated_workspace_mode,
            )
            .finish_non_exhaustive()
    }
}

impl ExecutionMetadata {
    /// The calling agent's sandbox id as a string, or `""` when unbound.
    #[must_use]
    pub fn sandbox_id_str(&self) -> &str {
        self.sandbox_id.as_ref().map_or("", SandboxId::as_str)
    }

    /// Require the bound sandbox id, else a framework fault.
    ///
    /// # Errors
    /// Returns [`ToolError::MissingContext`] when no sandbox is bound.
    pub fn require_sandbox_id(&self) -> Result<&SandboxId, ToolError> {
        self.sandbox_id
            .as_ref()
            .ok_or(ToolError::MissingContext("sandbox_id"))
    }

    /// Require the owning task id, else a framework fault.
    ///
    /// # Errors
    /// Returns [`ToolError::MissingContext`] when no task id is set.
    pub fn require_task_id(&self) -> Result<&TaskId, ToolError> {
        self.task_id
            .as_ref()
            .ok_or(ToolError::MissingContext("task_id"))
    }

    /// Require the owning request id, else a framework fault.
    ///
    /// # Errors
    /// Returns [`ToolError::MissingContext`] when no request id is set.
    pub fn require_request_id(&self) -> Result<&RequestId, ToolError> {
        self.request_id
            .as_ref()
            .ok_or(ToolError::MissingContext("request_id"))
    }

    /// Require the current agent-run id, else a framework fault.
    ///
    /// # Errors
    /// Returns [`ToolError::MissingContext`] when no agent-run id is set.
    pub fn require_agent_run_id(&self) -> Result<&AgentRunId, ToolError> {
        self.agent_run_id
            .as_ref()
            .ok_or(ToolError::MissingContext("agent_run_id"))
    }

    /// Require the owning attempt id, else a framework fault.
    ///
    /// # Errors
    /// Returns [`ToolError::MissingContext`] when no attempt id is set.
    pub fn require_attempt_id(&self) -> Result<&AttemptId, ToolError> {
        self.attempt_id
            .as_ref()
            .ok_or(ToolError::MissingContext("attempt_id"))
    }
}
