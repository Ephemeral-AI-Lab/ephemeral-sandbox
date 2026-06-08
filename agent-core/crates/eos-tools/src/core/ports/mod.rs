//! Transitional shared contracts while owner APIs replace the old ports crate.

use std::collections::BTreeMap;

use async_trait::async_trait;
use eos_state::{GeneratorSubmission, PlanDisposition, PlanNodeId, ReducerSubmission};
use eos_types::{AgentRunId, AttemptId, TaskId};

/// Agent-run transition contracts.
pub mod agent_run;
/// Sandbox command transition contracts.
pub mod command;
/// Workflow transition contracts.
pub mod workflow;

pub use agent_run::*;
pub use command::*;
pub use workflow::*;

/// Friend-seal for agent-core contract traits.
#[doc(hidden)]
pub trait Sealed {}

/// One planner-authored generator task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    /// Caller-assigned task id.
    pub id: PlanNodeId,
    /// Bound subagent profile name.
    pub agent_name: String,
    /// Ids this task depends on.
    pub needs: Vec<PlanNodeId>,
}

/// One planner-authored reducer task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReducer {
    /// Caller-assigned reducer id.
    pub id: PlanNodeId,
    /// Ids this reducer depends on.
    pub needs: Vec<PlanNodeId>,
    /// The reducer's instruction prompt.
    pub prompt: String,
}

/// A validated planner DAG submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerPlan {
    /// Owning attempt.
    pub attempt_id: AttemptId,
    /// The planner's own task.
    pub planner_task_id: TaskId,
    /// Whether the plan completes the attempt or defers a goal.
    pub disposition: PlanDisposition,
    /// The generator tasks, in submission order.
    pub tasks: Vec<PlanTask>,
    /// Per-task instruction specs, keyed by task id.
    pub task_specs: BTreeMap<PlanNodeId, String>,
    /// The reducer tasks, in submission order.
    pub reducers: Vec<PlanReducer>,
}

/// The result of applying a terminal submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionAck {
    /// The submission was accepted by the orchestrator.
    Accepted,
    /// The submission was rejected with a model-facing message.
    Rejected(String),
}

/// Per-attempt submission application for terminal tools.
#[async_trait]
pub trait AttemptSubmissionPort: Sealed + Send + Sync {
    /// Apply a validated planner DAG.
    async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, crate::ToolError>;

    /// Record one generator task's terminal outcome.
    async fn submit_generator(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<SubmissionAck, crate::ToolError>;

    /// Record one reducer task's terminal outcome.
    async fn apply_reducer(
        &self,
        submission: ReducerSubmission,
    ) -> Result<SubmissionAck, crate::ToolError>;
}

/// A system notification a tool/hook asks the engine to surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemNotification {
    /// The notification event key.
    pub event: String,
    /// Free-text body.
    pub message: String,
}

/// The engine notification service.
#[async_trait]
pub trait NotificationSink: Sealed + Send + Sync {
    /// Surface one system notification.
    async fn notify_system(&self, notification: SystemNotification) -> Result<(), crate::ToolError>;
}

/// A non-leaf effect a tool creates that must be torn down on cancellation.
#[async_trait]
pub trait CancelableResource: Send + Sync {
    /// Tear down the spawned effect.
    async fn teardown(&self, reason: &str) -> Result<(), crate::ToolError>;
}

/// Recursive agent-core cancellation primitives.
#[async_trait]
pub trait CancelPort: Send + Sync {
    /// Cancel a persisted task.
    async fn cancel_task(&self, task_id: &TaskId, reason: &str) -> Result<(), crate::ToolError>;

    /// Cancel a live agent run.
    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), crate::ToolError>;
}
