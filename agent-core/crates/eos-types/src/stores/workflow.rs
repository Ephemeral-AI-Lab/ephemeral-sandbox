//! Workflow-facing persistence contracts.

use async_trait::async_trait;

use crate::{
    AgentRunId, Attempt, AttemptBudget, AttemptClosure, AttemptId, CoreError, DeferredGoal,
    Iteration, IterationCreationReason, IterationId, IterationStatus, MaterializedPlan, RequestId,
    TaskId, ToolUseId, UtcDateTime, Workflow, WorkflowId, WorkflowStatus,
};

use super::Sealed;

/// Persistence surface for [`Workflow`].
#[async_trait]
pub trait WorkflowStore: Sealed + Send + Sync {
    /// Insert a fresh open workflow and return it.
    async fn insert(
        &self,
        request_id: &RequestId,
        parent_task_id: &TaskId,
        launched_by_agent_run_id: &AgentRunId,
        tool_use_id: Option<&ToolUseId>,
        workflow_goal: &str,
    ) -> Result<Workflow, CoreError>;

    /// Load a workflow by id.
    async fn get(&self, id: &WorkflowId) -> Result<Option<Workflow>, CoreError>;

    /// Append a child iteration id and return the updated workflow.
    async fn append_iteration_id(
        &self,
        id: &WorkflowId,
        iteration_id: &IterationId,
    ) -> Result<Workflow, CoreError>;

    /// Set status and optionally close time/outcomes.
    async fn set_status(
        &self,
        id: &WorkflowId,
        status: WorkflowStatus,
        closed_at: Option<UtcDateTime>,
        outcomes: Option<&str>,
    ) -> Result<Workflow, CoreError>;

    /// All workflows launched by one parent task, ordered by creation.
    async fn list_for_parent_task(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<Vec<Workflow>, CoreError>;

    /// All workflows launched by one agent run, ordered by creation.
    async fn list_for_launching_agent_run(
        &self,
        launched_by_agent_run_id: &AgentRunId,
    ) -> Result<Vec<Workflow>, CoreError>;
}

/// Persistence surface for [`Iteration`].
#[async_trait]
pub trait IterationStore: Sealed + Send + Sync {
    /// Insert a fresh open iteration and return it.
    async fn insert(
        &self,
        workflow_id: &WorkflowId,
        sequence_no: i64,
        creation_reason: IterationCreationReason,
        iteration_goal: &str,
        attempt_budget: AttemptBudget,
    ) -> Result<Iteration, CoreError>;

    /// Load an iteration by id.
    async fn get(&self, id: &IterationId) -> Result<Option<Iteration>, CoreError>;

    /// Append a child attempt id and return the updated iteration.
    async fn append_attempt_id(
        &self,
        id: &IterationId,
        attempt_id: &AttemptId,
    ) -> Result<Iteration, CoreError>;

    /// Set status and optionally close time/outcomes.
    async fn set_status(
        &self,
        id: &IterationId,
        status: IterationStatus,
        closed_at: Option<UtcDateTime>,
        outcomes: Option<&str>,
    ) -> Result<Iteration, CoreError>;

    /// Set the deferred-goal-for-next-iteration column.
    async fn set_deferred_goal_for_next_iteration(
        &self,
        id: &IterationId,
        deferred_goal_for_next_iteration: Option<&DeferredGoal>,
    ) -> Result<Iteration, CoreError>;

    /// Atomically transition to `succeeded` and write canonical outcomes.
    async fn close_succeeded(
        &self,
        id: &IterationId,
        outcomes: &str,
        closed_at: Option<UtcDateTime>,
    ) -> Result<Iteration, CoreError>;

    /// All iterations of a workflow, ordered by sequence number.
    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Iteration>, CoreError>;
}

/// Persistence surface for [`Attempt`].
#[async_trait]
pub trait AttemptStore: Sealed + Send + Sync {
    /// Insert a fresh attempt in the planning stage and return it.
    async fn insert(
        &self,
        iteration_id: &IterationId,
        workflow_id: &WorkflowId,
        attempt_sequence_no: i64,
    ) -> Result<Attempt, CoreError>;

    /// Load an attempt by id.
    async fn get(&self, id: &AttemptId) -> Result<Option<Attempt>, CoreError>;

    /// Record the planner task assigned to this attempt.
    async fn record_planner_task(
        &self,
        id: &AttemptId,
        planner_task_id: &TaskId,
    ) -> Result<Attempt, CoreError>;

    /// Record a materialized planner DAG and transition the attempt to run.
    async fn record_plan(
        &self,
        id: &AttemptId,
        plan: &MaterializedPlan,
    ) -> Result<Attempt, CoreError>;

    /// Close the attempt with a typed terminal closure.
    async fn close(&self, id: &AttemptId, closure: AttemptClosure) -> Result<Attempt, CoreError>;

    /// All attempts of an iteration, ordered by attempt sequence number.
    async fn list_for_iteration(
        &self,
        iteration_id: &IterationId,
    ) -> Result<Vec<Attempt>, CoreError>;
}
