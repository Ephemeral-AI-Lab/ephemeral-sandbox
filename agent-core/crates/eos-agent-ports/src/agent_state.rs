//! Agent runtime snapshot for metadata and audit rendering.

use eos_types::{AgentRunId, AttemptId, IterationId, RequestId, SandboxId, TaskId, WorkflowId};

/// Current runtime metadata facts for one agent run.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// Agent-run id.
    pub agent_run_id: AgentRunId,
    /// Bound agent profile name.
    pub agent_name: String,
    /// Owning request id.
    pub request_id: Option<RequestId>,
    /// Owning task id.
    pub task_id: Option<TaskId>,
    /// Owning workflow id.
    pub workflow_id: Option<WorkflowId>,
    /// Owning workflow iteration id.
    pub iteration_id: Option<IterationId>,
    /// Owning attempt id.
    pub attempt_id: Option<AttemptId>,
    /// Bound sandbox id.
    pub sandbox_id: Option<SandboxId>,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Whether the run currently has an isolated workspace open.
    pub is_isolated_workspace_mode: bool,
}
