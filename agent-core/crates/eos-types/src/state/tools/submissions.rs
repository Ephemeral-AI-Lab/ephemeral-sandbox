//! Validated terminal-outcome submission DTOs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AttemptId, DeferredGoal, TaskId, WorkItemId, WorkItemSpec};

/// Model-facing pass/fail status used by terminal outcome tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    /// Terminal reports a pass.
    Success,
    /// Terminal reports a failure.
    Failed,
}

impl SubmissionStatus {
    /// Whether this status maps to a passing outcome.
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Success)
    }

    /// The canonical `snake_case` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

/// Validated planner plan submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlanOutcomeSubmission {
    /// Owning attempt.
    pub attempt_id: AttemptId,
    /// Planner-level explanation of the work item plan.
    pub plan_spec: String,
    /// Planner-authored work items.
    pub work_items: Vec<WorkItemSpec>,
    /// Concrete current-iteration goal items carried to the next iteration.
    #[serde(default)]
    pub deferred_goal_for_next_iteration: Option<DeferredGoal>,
}

/// Validated terminal outcome for one worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutcomeSubmission {
    /// Owning attempt.
    pub attempt_id: AttemptId,
    /// Opaque worker task id.
    pub task_id: TaskId,
    /// Planner-authored work item id.
    pub work_item_id: WorkItemId,
    /// Success or failure.
    pub status: SubmissionStatus,
    /// Natural-language deliverable or blocker.
    pub outcome: String,
}
