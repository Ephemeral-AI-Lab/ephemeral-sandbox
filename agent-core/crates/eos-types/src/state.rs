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
    BackgroundSessionCounts, PlanOutcomeSubmission, SubmissionStatus, WorkerOutcomeSubmission,
};
pub use workflow::{
    AdvisorVerdict, Attempt, AttemptBudget, AttemptClosure, AttemptExecutionTree,
    AttemptFailReason, AttemptOutcome, AttemptStage, AttemptState, AttemptStatus, DeferredGoal,
    ExecutionNode, Iteration, IterationCreationReason, IterationOutcome, IterationStatus,
    ParentedOutcome, PlanId, PlannerOutcome, TaskOutcome, WorkItemId, WorkItemSpec, WorkerOutcome,
    Workflow, WorkflowOutcome, WorkflowStatus, NO_OUTCOME,
};
