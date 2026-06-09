//! eos-types — shared id, timestamp, clock, json, errors, and state contracts.
//!
//! This is the upstream contract crate of the agent-core dependency DAG. It
//! holds the eleven typed string ids, the [`UtcDateTime`] wrapper, the [`Clock`]
//! seam, the transitional [`JsonObject`] alias, the minimal [`CoreError`], and
//! the persisted DTO/store contracts shared across runtime, engine, workflow,
//! tools, and database crates. It deliberately holds no SQL, HTTP, provider, or
//! orchestration behavior.
//!
//! The public surface is re-exported flatly, so consumers write
//! `use eos_types::{TaskId, UtcDateTime, Clock, JsonObject};`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod contracts;
mod error;
mod frontmatter;
mod ids;
mod json;
mod llm;
pub mod ports;
pub mod state;
mod time;

pub use agent::{
    AgentDefinition, AgentName, AgentNameError, AgentRegistry, AgentRegistryBuilder, AgentType,
};
pub use contracts::{
    AgentRunApi, AgentRunError, AgentRunMessageRecordKind, AgentRunOutcome, AgentRunStatus,
    AgentState, AttemptSubmissionPort, CancelError, CancelPort, OutstandingWorkflow, PlanReducer,
    PlanTask, PlannerPlan, SpawnAgentRequest, StartWorkflowRequest, StartedWorkflow, SubmissionAck,
    TerminalWorkflow, WorkflowApi, WorkflowApiError, WorkflowTaskRole, WorkflowTerminalStatus,
};
pub use error::CoreError;
pub use frontmatter::parse_markdown_frontmatter;
pub use ids::{
    AgentRunId, AttemptId, CommandSessionId, InvocationId, IterationId, RequestId, SandboxId,
    TaskId, ToolUseId, WorkflowId,
};
pub use json::JsonObject;
pub use llm::{ContentBlock, Message, MessageRole, ToolSpec};
pub use ports::{
    AgentRunStore, AttemptStore, IterationStore, ModelStore, RequestStore, Sealed, StoreError,
    TaskStore, WorkflowStore,
};
pub use state::{
    execution_outcome_for_submission, present_status, AgentRun, Attempt, AttemptBudget,
    AttemptClosure, AttemptFailReason, AttemptStage, AttemptState, AttemptStatus, DeferredGoal,
    ExecutionRole, ExecutionTaskOutcome, GeneratorSubmission, Iteration, IterationCreationReason,
    IterationOutcome, IterationStatus, MaterializedPlan, ModelRegistration, Page, PageResult,
    PlanDisposition, PlanNodeId, PlannerFailReason, PlannerFailureSubmission, PlannerSubmission,
    ReducerSubmission, Request, RequestListFilter, RequestStatus, Task, TaskOutcomeStatus,
    TaskRole, TaskStatus, Workflow, WorkflowOutcome, WorkflowStatus, NO_OUTCOME, TASK_AGENT_ROLES,
};
pub use time::{Clock, SystemClock, TestClock, UtcDateTime};
