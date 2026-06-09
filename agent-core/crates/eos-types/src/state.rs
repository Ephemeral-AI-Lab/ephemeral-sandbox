//! Shared persisted state DTOs grouped by their behavior owner.

pub mod engine;
pub mod model_registry;
pub mod request_task;
pub mod tools;
pub mod workflow;

pub use engine::AgentRun;
pub use model_registry::ModelRegistration;
pub use request_task::{
    ParentedRun, RunningRequestAgentRun, Task, TaskRole, TaskRun, TaskStatus, TASK_AGENT_ROLES,
};
pub use request_task::{Request, RequestStatus};
pub use tools::{
    BackgroundSessionCounts, GeneratorSubmission, PlannerFailReason, PlannerFailureSubmission,
    PlannerSubmission, ReducerSubmission,
};
pub use workflow::{
    execution_outcome_for_submission, present_status, Attempt, AttemptBudget, AttemptClosure,
    AttemptFailReason, AttemptStage, AttemptState, AttemptStatus, DeferredGoal, ExecutionRole,
    ExecutionTaskOutcome, GeneratorId, Iteration, IterationCreationReason, IterationOutcome,
    IterationStatus, MaterializedPlan, PlanDisposition, PlannerId, ReducerId, TaskOutcomeStatus,
    Workflow, WorkflowOutcome, WorkflowStatus, NO_OUTCOME,
};
