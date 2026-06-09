//! `Attempt` lifecycle DTOs and execution-tree bindings.

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::{AttemptId, IterationId, PlanId, TaskId, UtcDateTime, WorkItemId, WorkflowId};

/// Validated attempt budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AttemptBudget(NonZeroU32);

impl AttemptBudget {
    /// Construct a budget from a nonzero count.
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    /// Try to construct a budget from a `u32`.
    ///
    /// # Errors
    /// Returns [`CoreError`] when the count is zero.
    pub fn try_from_u32(value: u32) -> Result<Self, CoreError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or_else(|| CoreError::Store("attempt budget must be greater than zero".to_owned()))
    }

    /// Try to construct a budget from the database integer representation.
    ///
    /// # Errors
    /// Returns [`CoreError`] when the count is zero, negative, or too large.
    pub fn try_from_i64(value: i64) -> Result<Self, CoreError> {
        let value = u32::try_from(value).map_err(|_| {
            CoreError::Store("attempt budget must fit u32 and be greater than zero".to_owned())
        })?;
        Self::try_from_u32(value)
    }

    /// Return the budget as a plain count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// Return the database integer representation.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self.0.get() as i64
    }
}

impl Default for AttemptBudget {
    fn default() -> Self {
        Self(NonZeroU32::new(2).unwrap_or(NonZeroU32::MIN))
    }
}

impl TryFrom<u32> for AttemptBudget {
    type Error = CoreError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::try_from_u32(value)
    }
}

impl TryFrom<i64> for AttemptBudget {
    type Error = CoreError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::try_from_i64(value)
    }
}

/// Stage of an [`Attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStage {
    /// Planning the work-item plan.
    Plan,
    /// Running planned work items.
    Run,
    /// Closed (terminal).
    Closed,
}

/// Outcome status of an [`Attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    /// In progress.
    Running,
    /// Passed.
    Passed,
    /// Failed.
    Failed,
    /// Cancelled before reaching a natural terminal.
    Cancelled,
}

impl AttemptStatus {
    /// The canonical `snake_case` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Why an attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptFailReason {
    /// A worker task in the plan failed.
    TaskFailed,
    /// The attempt failed to start up.
    StartupFailed,
}

/// Terminal closure of an [`Attempt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptClosure {
    /// All workers passed.
    Passed {
        /// Close timestamp.
        closed_at: UtcDateTime,
    },
    /// Attempt failed.
    Failed {
        /// Required failure reason.
        reason: AttemptFailReason,
        /// Close timestamp.
        closed_at: UtcDateTime,
    },
    /// Attempt was cancelled.
    Cancelled {
        /// Cancellation reason.
        reason: String,
        /// Close timestamp.
        closed_at: UtcDateTime,
    },
}

impl AttemptClosure {
    /// Closure status.
    #[must_use]
    pub const fn status(&self) -> AttemptStatus {
        match self {
            Self::Passed { .. } => AttemptStatus::Passed,
            Self::Failed { .. } => AttemptStatus::Failed,
            Self::Cancelled { .. } => AttemptStatus::Cancelled,
        }
    }

    /// Failure reason, if failed.
    #[must_use]
    pub const fn fail_reason(&self) -> Option<AttemptFailReason> {
        match self {
            Self::Passed { .. } | Self::Cancelled { .. } => None,
            Self::Failed { reason, .. } => Some(*reason),
        }
    }

    /// Close timestamp.
    #[must_use]
    pub const fn closed_at(&self) -> UtcDateTime {
        match self {
            Self::Passed { closed_at }
            | Self::Failed { closed_at, .. }
            | Self::Cancelled { closed_at, .. } => *closed_at,
        }
    }
}

/// Attempt lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// Planner has not materialized a plan yet.
    Planning {
        /// Whether the planner run has been spawned.
        started: bool,
    },
    /// Planner has materialized an execution tree and workers may run.
    Running,
    /// Attempt is terminal.
    Closed {
        /// Terminal closure.
        closure: AttemptClosure,
    },
}

impl AttemptState {
    /// Persisted stage view derived from the state.
    #[must_use]
    pub const fn stage(&self) -> AttemptStage {
        match self {
            Self::Planning { .. } => AttemptStage::Plan,
            Self::Running => AttemptStage::Run,
            Self::Closed { .. } => AttemptStage::Closed,
        }
    }

    /// Persisted status view derived from the state.
    #[must_use]
    pub const fn status(&self) -> AttemptStatus {
        match self {
            Self::Planning { .. } | Self::Running => AttemptStatus::Running,
            Self::Closed { closure } => closure.status(),
        }
    }

    /// Terminal closure, if closed.
    #[must_use]
    pub const fn closure(&self) -> Option<&AttemptClosure> {
        match self {
            Self::Closed { closure } => Some(closure),
            Self::Planning { .. } | Self::Running => None,
        }
    }
}

/// Attempt-owned mapping from planner/work item ids to opaque task ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttemptExecutionTree {
    /// Attempt-local plan id.
    pub plan_id: PlanId,
    /// Opaque planner task id, bound when the planner is spawned.
    #[serde(default)]
    pub planner_task_id: Option<TaskId>,
    /// Work item nodes materialized when the planner submits a plan.
    #[serde(default)]
    pub nodes: Vec<ExecutionNode>,
}

impl AttemptExecutionTree {
    /// Create an empty execution tree for a freshly minted plan id.
    #[must_use]
    pub fn new(plan_id: PlanId) -> Self {
        Self {
            plan_id,
            planner_task_id: None,
            nodes: Vec::new(),
        }
    }

    /// All bound worker task ids.
    #[must_use]
    pub fn worker_task_ids(&self) -> Vec<TaskId> {
        self.nodes
            .iter()
            .filter_map(|node| node.task_id.clone())
            .collect()
    }

    /// Find one execution node by work item id.
    #[must_use]
    pub fn node(&self, work_item_id: &WorkItemId) -> Option<&ExecutionNode> {
        self.nodes
            .iter()
            .find(|node| &node.work_item_id == work_item_id)
    }
}

/// One work item execution binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionNode {
    /// Planner-authored work item id.
    pub work_item_id: WorkItemId,
    /// Direct work item dependencies.
    #[serde(default)]
    pub needs: Vec<WorkItemId>,
    /// Opaque worker task id, bound when this work item is spawned.
    #[serde(default)]
    pub task_id: Option<TaskId>,
}

/// Immutable view of a persisted Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attempt {
    /// Attempt identifier.
    pub id: AttemptId,
    /// Owning iteration.
    pub iteration_id: IterationId,
    /// Owning workflow.
    pub workflow_id: WorkflowId,
    /// Monotonic per-iteration sequence number.
    pub attempt_sequence_no: i64,
    /// Attempt-local plan id.
    pub plan_id: PlanId,
    /// Attempt↔task and `work_item`↔task index.
    pub execution_tree: AttemptExecutionTree,
    /// Lifecycle state.
    pub state: AttemptState,
    /// Creation timestamp.
    pub created_at: UtcDateTime,
    /// Last-update timestamp.
    pub updated_at: UtcDateTime,
}

impl Attempt {
    /// Persisted stage view.
    #[must_use]
    pub const fn stage(&self) -> AttemptStage {
        self.state.stage()
    }

    /// Persisted status view.
    #[must_use]
    pub const fn status(&self) -> AttemptStatus {
        self.state.status()
    }

    /// Whether the attempt has reached the closed stage.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self.state, AttemptState::Closed { .. })
    }

    /// Planner task id, if bound.
    #[must_use]
    pub const fn planner_task_id(&self) -> Option<&TaskId> {
        self.execution_tree.planner_task_id.as_ref()
    }

    /// Bound worker task ids.
    #[must_use]
    pub fn worker_task_ids(&self) -> Vec<TaskId> {
        self.execution_tree.worker_task_ids()
    }

    /// Terminal closure, if closed.
    #[must_use]
    pub const fn closure(&self) -> Option<&AttemptClosure> {
        self.state.closure()
    }

    /// Failure reason, if failed.
    #[must_use]
    pub const fn fail_reason(&self) -> Option<AttemptFailReason> {
        match self.closure() {
            Some(closure) => closure.fail_reason(),
            None => None,
        }
    }

    /// Close timestamp, if closed.
    #[must_use]
    pub const fn closed_at(&self) -> Option<UtcDateTime> {
        match self.closure() {
            Some(closure) => Some(closure.closed_at()),
            None => None,
        }
    }
}
