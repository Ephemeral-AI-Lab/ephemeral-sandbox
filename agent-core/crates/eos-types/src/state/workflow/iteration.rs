//! `Iteration` DTO (vertical-continuation axis) and its enums.
//!
//! Ports the Iteration half of `workflow/_core/state.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AttemptBudget, AttemptId, IterationId, UtcDateTime, WorkflowId};

/// Lifecycle status of an [`Iteration`] (Rust `IterationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IterationStatus {
    /// Running; not yet closed.
    Open,
    /// Closed successfully.
    Succeeded,
    /// Closed with failure.
    Failed,
    /// Closed by cancellation.
    Cancelled,
}

/// Why an iteration was created (Rust `IterationCreationReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IterationCreationReason {
    /// The first iteration of a workflow.
    Initial,
    /// A continuation iteration spawned from a prior iteration's deferred goal.
    DeferredGoalContinuation,
}

/// Immutable view of a persisted Iteration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Iteration {
    /// Iteration identifier.
    pub id: IterationId,
    /// Owning workflow.
    pub workflow_id: WorkflowId,
    /// Monotonic per-workflow sequence number (unique).
    pub sequence_no: i64,
    /// Why this iteration was created.
    pub creation_reason: IterationCreationReason,
    /// Denormalized workflow goal.
    pub workflow_goal: String,
    /// This iteration's own goal.
    pub iteration_goal: String,
    /// Maximum number of attempts allowed in this iteration.
    pub attempt_budget: AttemptBudget,
    /// Lifecycle status.
    pub status: IterationStatus,
    /// Ordered child attempt ids.
    pub attempt_ids: Vec<AttemptId>,
    /// Creation timestamp.
    pub created_at: UtcDateTime,
    /// Last-update timestamp.
    pub updated_at: UtcDateTime,
    /// Close timestamp, if closed.
    pub closed_at: Option<UtcDateTime>,
}

impl Iteration {
    /// Whether the iteration is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self.status, IterationStatus::Open)
    }

    /// Number of attempts created so far.
    #[must_use]
    pub fn attempt_count(&self) -> usize {
        self.attempt_ids.len()
    }

    /// Whether the attempt budget still allows another attempt.
    #[must_use]
    pub fn has_budget_remaining(&self) -> bool {
        self.attempt_count() < self.attempt_budget.get() as usize
    }

    /// The most recently created attempt id, if any.
    #[must_use]
    pub fn latest_attempt_id(&self) -> Option<&AttemptId> {
        self.attempt_ids.last()
    }
}
