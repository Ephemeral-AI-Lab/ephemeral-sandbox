//! Per-run terminal outcomes and read-side workflow outcome projections.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DeferredGoal, WorkItemSpec};

/// Placeholder text for missing model-authored terminal detail.
pub const NO_OUTCOME: &str = "(no outcome recorded)";

/// Planner outcome payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlannerOutcome {
    /// Planner-level explanation of the work item plan.
    pub plan_spec: String,
    /// Planner-authored work items.
    pub work_items: Vec<WorkItemSpec>,
    /// Concrete current-iteration goal items carried to the next iteration.
    #[serde(default)]
    pub deferred_goal_for_next_iteration: Option<DeferredGoal>,
}

/// Worker outcome payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutcome {
    /// Whether this worker passed.
    pub is_pass: bool,
    /// Natural-language deliverable or blocker.
    pub outcome: String,
}

/// Workflow-task family outcome for root, planner, and worker agent rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskOutcome {
    /// Root request result.
    Root {
        /// Whether the request passed.
        is_pass: bool,
        /// Natural-language result.
        outcome: String,
    },
    /// Planner's full plan.
    Planner {
        /// Planner-level explanation of the work item plan.
        plan_spec: String,
        /// Planner-authored work items.
        work_items: Vec<WorkItemSpec>,
        /// Concrete current-iteration goal items carried to the next iteration.
        #[serde(default)]
        deferred_goal_for_next_iteration: Option<DeferredGoal>,
    },
    /// One worker's deliverable or blocker.
    Worker {
        /// Whether this worker passed.
        is_pass: bool,
        /// Natural-language deliverable or blocker.
        outcome: String,
    },
}

impl TaskOutcome {
    /// Build a planner payload from the planner variant.
    #[must_use]
    pub fn planner_outcome(&self) -> Option<PlannerOutcome> {
        match self {
            Self::Planner {
                plan_spec,
                work_items,
                deferred_goal_for_next_iteration,
            } => Some(PlannerOutcome {
                plan_spec: plan_spec.clone(),
                work_items: work_items.clone(),
                deferred_goal_for_next_iteration: deferred_goal_for_next_iteration.clone(),
            }),
            Self::Root { .. } | Self::Worker { .. } => None,
        }
    }

    /// Build a worker payload from the worker variant.
    #[must_use]
    pub fn worker_outcome(&self) -> Option<WorkerOutcome> {
        match self {
            Self::Worker { is_pass, outcome } => Some(WorkerOutcome {
                is_pass: *is_pass,
                outcome: outcome.clone(),
            }),
            Self::Root { .. } | Self::Planner { .. } => None,
        }
    }
}

/// Advisor verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorVerdict {
    /// Approve the proposed action.
    Approve,
    /// Reject the proposed action.
    Reject,
}

/// Parented family outcome for advisor and subagent rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentedOutcome {
    /// Advisor terminal verdict.
    Advisor {
        /// Advisor verdict.
        verdict: AdvisorVerdict,
        /// Natural-language review outcome.
        outcome: String,
    },
    /// Subagent terminal result.
    Subagent {
        /// Natural-language result.
        outcome: String,
    },
}

/// Read-side projection for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttemptOutcome {
    /// Planner returned a plan and every worker passed.
    pub status: bool,
    /// Planner's full plan outcome.
    pub planner_outcome: PlannerOutcome,
    /// Worker outcomes, in execution-tree order.
    pub worker_outcomes: Vec<WorkerOutcome>,
}

/// Read-side projection for one iteration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IterationOutcome {
    /// Terminal returned-attempt status.
    pub status: bool,
    /// Deferred goal from the returned attempt's planner outcome.
    #[serde(default)]
    pub deferred_goal: Option<DeferredGoal>,
    /// Attempt projections.
    pub attempts: Vec<AttemptOutcome>,
}

/// Read-side projection for one workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowOutcome {
    /// Terminal returned-iteration status.
    pub status: bool,
    /// Iteration projections.
    pub iterations: Vec<IterationOutcome>,
}
