//! eos-state — pure agent-core domain state, outcome projections, terminal
//! submission DTOs, and the per-entity async `Store` traits.
//!
//! This is the upstream domain contract that `eos-db` implements and that
//! `eos-tools`/`eos-engine`/`eos-workflow`/`eos-runtime` consume. It defines
//! *what is stored and what shapes flow between layers*; it never executes I/O.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent_run;
mod attempt;
mod iteration;
mod model;
mod outcomes;
mod pagination;
mod plan;
mod request;
mod store;
mod submissions;
mod task;
mod workflow;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

pub use agent_run::AgentRun;
pub use attempt::{
    Attempt, AttemptClosure, AttemptFailReason, AttemptStage, AttemptState, AttemptStatus,
};
pub use iteration::{Iteration, IterationCreationReason, IterationOutcome, IterationStatus};
pub use model::ModelRegistration;
pub use outcomes::{
    attempt_execution_outcomes, execution_outcome_for_submission, latest_iteration, present_status,
    project_attempt_outcomes, project_iteration_outcomes, ExecutionRole, ExecutionTaskOutcome,
    TaskOutcomeStatus, NO_OUTCOME,
};
pub use pagination::{Page, PageResult, RequestListFilter};
pub use plan::{AttemptBudget, DeferredGoal, MaterializedPlan, PlanDisposition, PlanNodeId};
pub use request::{Request, RequestStatus};
pub use store::{
    AgentRunStore, AttemptStore, IterationStore, ModelStore, RequestStore, Sealed, StoreError,
    TaskStore, WorkflowStore,
};
pub use submissions::{
    GeneratorSubmission, PlannerFailReason, PlannerFailureSubmission, PlannerSubmission,
    ReducerSubmission,
};
pub use task::{Task, TaskRole, TaskStatus, TASK_AGENT_ROLES};
pub use workflow::{Workflow, WorkflowOutcome, WorkflowStatus};

// Re-export the upstream value primitives that appear in this crate's public
// API so downstream crates (notably `eos-db`) can name them without a direct
// `eos-types` dependency edge, preserving the `eos-db -> {state, config}` topology.
pub use eos_types::{
    AgentRunId, AttemptId, CoreError, IterationId, JsonObject, RequestId, SandboxId, TaskId,
    UtcDateTime, WorkflowId,
};

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod tests;
