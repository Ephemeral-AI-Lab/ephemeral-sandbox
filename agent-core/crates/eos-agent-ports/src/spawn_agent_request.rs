//! Agent spawn request DTOs.

use eos_llm_client::Message;
use eos_types::{AgentRunId, AttemptId, IterationId, RequestId, SandboxId, TaskId, WorkflowId};

use crate::AgentName;

/// Request to spawn any agent kind.
#[derive(Debug, Clone)]
pub struct SpawnAgentRequest {
    /// Agent profile name to launch.
    pub agent_name: AgentName,
    /// Optional caller-provided run id; one is minted when absent.
    pub agent_run_id: Option<AgentRunId>,
    /// Initial transcript.
    pub initial_messages: Vec<Message>,
    /// Parent agent-run id, for helper/subagent lineage.
    pub parent_agent_run_id: Option<AgentRunId>,
    /// Owning request id.
    pub request_id: Option<RequestId>,
    /// Owning task id.
    pub task_id: Option<TaskId>,
    /// Owning attempt id.
    pub attempt_id: Option<AttemptId>,
    /// Owning workflow id.
    pub workflow_id: Option<WorkflowId>,
    /// Bound sandbox id.
    pub sandbox_id: Option<SandboxId>,
    /// Request-visible workspace root.
    pub workspace_root: String,
    /// Whether the caller is in isolated-workspace mode.
    pub is_isolated_workspace_mode: bool,
    /// Whether to persist the run row.
    pub persist: bool,
    /// Message-record kind.
    pub record_kind: AgentRunMessageRecordKind,
}

/// Agent-run message-record layout choice carried by callers without exposing
/// `eos-agent-message-records` outside the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunMessageRecordKind {
    /// Root request agent.
    Root,
    /// Delegated workflow planner/generator/reducer task agent.
    WorkflowTask {
        /// Owning workflow id.
        workflow_id: WorkflowId,
        /// Owning iteration id.
        iteration_id: IterationId,
        /// Owning attempt id.
        attempt_id: AttemptId,
        /// Workflow task role.
        role: WorkflowTaskRole,
    },
    /// Background subagent run under a parent agent.
    Subagent {
        /// Parent agent-run id.
        parent_agent_run_id: AgentRunId,
    },
    /// Advisor run under a parent agent.
    Advisor {
        /// Parent agent-run id.
        parent_agent_run_id: AgentRunId,
    },
    /// Generic agent run when no narrower layout is known.
    Agent,
}

/// Backwards-compatible short name while callers migrate.
pub use AgentRunMessageRecordKind as AgentRunRecordKind;

/// Workflow task role used for message-record path labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowTaskRole {
    /// Planner task.
    Planner,
    /// Generator task.
    Generator,
    /// Reducer task.
    Reducer,
}
