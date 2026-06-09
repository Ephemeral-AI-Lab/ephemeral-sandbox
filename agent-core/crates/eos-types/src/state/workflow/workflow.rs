//! `Workflow` lifecycle DTO and status.
//!
//! Ports the Workflow half of `workflow/_core/state.py`. `parent_task_id` is a
//! durable back-link and is **never** mutated at close (anchor §3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentRunId, IterationId, RequestId, TaskId, ToolUseId, UtcDateTime, WorkflowId};

/// Lifecycle status of a [`Workflow`] (Rust `WorkflowStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Running; not yet closed.
    Open,
    /// Closed successfully.
    Succeeded,
    /// Closed with failure.
    Failed,
    /// Closed by cancellation.
    Cancelled,
}

/// Immutable view of a persisted Workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Workflow {
    /// Workflow identifier.
    pub id: WorkflowId,
    /// Owning request.
    pub request_id: RequestId,
    /// The workflow goal.
    pub workflow_goal: String,
    /// Lifecycle status.
    pub status: WorkflowStatus,
    /// Ordered child iteration ids.
    pub iteration_ids: Vec<IterationId>,
    /// The launching task; durable back-link, never mutated at close.
    pub parent_task_id: TaskId,
    /// Agent run that launched this workflow.
    pub parent_agent_run_id: AgentRunId,
    /// Tool use that launched this workflow, if available.
    #[serde(default)]
    pub tool_use_id: Option<ToolUseId>,
    /// Creation timestamp.
    pub created_at: UtcDateTime,
    /// Last-update timestamp.
    pub updated_at: UtcDateTime,
    /// Close timestamp, if closed.
    pub closed_at: Option<UtcDateTime>,
}

impl Workflow {
    /// Whether the workflow is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self.status, WorkflowStatus::Open)
    }
}
