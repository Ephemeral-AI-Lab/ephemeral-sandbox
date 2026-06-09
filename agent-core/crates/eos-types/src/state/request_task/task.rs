//! `Task` — the persisted agent interface — with its status and role vocabularies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AgentName, AgentRunId, JsonObject, ParentedAgentRunKind, ParentedOutcome, RequestId, TaskId,
    TaskOutcome, ToolUseId, UtcDateTime,
};

/// Lifecycle status of a persisted [`Task`] (Rust `TaskStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Created, not yet started.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Done,
    /// Completed with failure.
    Failed,
    /// Could not proceed (blocked on an unmet dependency).
    Blocked,
    /// Cancelled before reaching a natural terminal. Blocks DAG descendants the
    /// same way `Failed` does.
    Cancelled,
}

impl TaskStatus {
    /// Whether this is a terminal task status.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Blocked | Self::Cancelled
        )
    }
}

/// The persisted task roles for root, planner, and worker agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskRole {
    /// Root request agent.
    Root,
    /// Planner agent authoring an attempt plan.
    Planner,
    /// Worker agent executing one planner-authored work item.
    Worker,
}

impl TaskRole {
    /// The canonical `snake_case` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Planner => "planner",
            Self::Worker => "worker",
        }
    }
}

/// The persisted task roles, mirroring Rust `TASK_AGENT_ROLES`.
pub const TASK_AGENT_ROLES: [TaskRole; 3] = [TaskRole::Root, TaskRole::Planner, TaskRole::Worker];

/// Immutable view of a persisted task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// Opaque task identifier, minted by the task store.
    pub id: TaskId,
    /// Owning request.
    pub request_id: RequestId,
    /// Agent role.
    pub role: TaskRole,
    /// Instruction text the agent runs against.
    pub instruction: String,
    /// Lifecycle status.
    pub status: TaskStatus,
    /// Bound agent profile name, if assigned.
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Typed terminal outcome, if a terminal has stamped one.
    #[serde(default)]
    pub task_outcome: Option<TaskOutcome>,
}

/// Persisted root/planner/worker task-agent-run row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRun {
    /// Schedulable task identity.
    pub task_id: TaskId,
    /// Agent-run execution and record identity.
    pub agent_run_id: AgentRunId,
    /// Owning request.
    pub request_id: RequestId,
    /// Workflow role for this task-agent-run.
    pub role: TaskRole,
    /// Lifecycle status.
    pub status: TaskStatus,
    /// Bound agent profile.
    pub agent_name: AgentName,
    /// Raw terminal payload projection, if any.
    #[serde(default)]
    pub terminal_payload: Option<JsonObject>,
    /// Typed mirror of the terminal payload, if any.
    #[serde(default)]
    pub task_outcome: Option<TaskOutcome>,
    /// Provider token count.
    pub token_count: i64,
    /// Terminal error summary, if any.
    #[serde(default)]
    pub error: Option<String>,
    /// Creation timestamp.
    pub created_at: UtcDateTime,
    /// Last-update timestamp.
    pub updated_at: UtcDateTime,
    /// Finish timestamp, if terminal.
    #[serde(default)]
    pub finished_at: Option<UtcDateTime>,
}

/// Running agent-run lineage row used for request-scoped cancellation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunningRequestAgentRun {
    /// Owning request.
    pub request_id: RequestId,
    /// Task identity bound to the agent run.
    pub task_id: TaskId,
    /// Agent-run execution identity.
    pub agent_run_id: AgentRunId,
    /// Current running status.
    pub status: TaskStatus,
}

/// Parent-launched task-backed subagent/advisor run row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParentedRun {
    /// Own task identity.
    pub task_id: TaskId,
    /// Agent-run execution and record identity.
    pub agent_run_id: AgentRunId,
    /// Owning request.
    pub request_id: RequestId,
    /// Lifecycle status.
    pub status: TaskStatus,
    /// Exact parent agent run that launched this run.
    pub parent_agent_run_id: AgentRunId,
    /// Denormalized parent task grouping index.
    pub parent_task_id: TaskId,
    /// Parent-launched run kind.
    pub kind: ParentedAgentRunKind,
    /// Model tool-use id that launched this run, if available.
    #[serde(default)]
    pub tool_use_id: Option<ToolUseId>,
    /// Bound agent profile.
    pub agent_name: AgentName,
    /// Terminal payload projection, if any.
    #[serde(default)]
    pub terminal_payload: Option<JsonObject>,
    /// Typed mirror of the parented terminal payload, if any.
    #[serde(default)]
    pub parented_outcome: Option<ParentedOutcome>,
    /// Provider token count.
    pub token_count: i64,
    /// Terminal error summary, if any.
    #[serde(default)]
    pub error: Option<String>,
    /// Creation timestamp.
    pub created_at: UtcDateTime,
    /// Last-update timestamp.
    pub updated_at: UtcDateTime,
    /// Finish timestamp, if terminal.
    #[serde(default)]
    pub finished_at: Option<UtcDateTime>,
}
