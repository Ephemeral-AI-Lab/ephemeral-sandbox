//! Shared async persistence stores grouped by consuming behavior boundary.

mod engine {
    //! Engine-facing agent-run persistence contracts.

    use async_trait::async_trait;

    use crate::{AgentRun, AgentRunId, CoreError, JsonObject, TaskId};

    use super::Sealed;

    /// Persistence surface for [`AgentRun`].
    #[async_trait]
    pub trait AgentRunStore: Sealed + Send + Sync {
        /// Create a run row with only the create-time fields set.
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: Option<&TaskId>,
            agent_name: &str,
        ) -> Result<AgentRun, CoreError>;

        /// Write the finish-time fields. `Ok(None)` means the run does not exist.
        async fn finish_run(
            &self,
            agent_run_id: &AgentRunId,
            terminal_payload: Option<&JsonObject>,
            token_count: i64,
            error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError>;

        /// Load a run by id.
        async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError>;

        /// The latest agent run for one task, if any.
        async fn get_for_task(&self, task_id: &TaskId) -> Result<Option<AgentRun>, CoreError>;
    }
}
mod model_registry {
    //! Model-registry persistence contracts.

    use async_trait::async_trait;

    use crate::{CoreError, JsonObject, ModelRegistration};

    use super::Sealed;

    /// Persistence surface for [`ModelRegistration`].
    #[async_trait]
    pub trait ModelStore: Sealed + Send + Sync {
        /// Create or update a registration.
        async fn register(
            &self,
            model_key: &str,
            label: &str,
            class_path: &str,
            kwargs: &JsonObject,
            activate: bool,
        ) -> Result<ModelRegistration, CoreError>;

        /// Delete by key; `Ok(false)` means no such key.
        async fn delete(&self, model_key: &str) -> Result<bool, CoreError>;

        /// Load a registration by key.
        async fn get(&self, model_key: &str) -> Result<Option<ModelRegistration>, CoreError>;

        /// The single active registration, if any.
        async fn active(&self) -> Result<Option<ModelRegistration>, CoreError>;
    }
}
mod request_task {
    //! Runtime-facing request and task persistence contracts.

    use async_trait::async_trait;

    use crate::{
        AttemptId, CoreError, ExecutionTaskOutcome, JsonObject, Request, RequestId, RequestStatus,
        SandboxId, Task, TaskId, TaskStatus,
    };

    use super::Sealed;

    /// Persistence surface for request/task rows.
    #[async_trait]
    pub trait TaskStore: Sealed + Send + Sync {
        /// Insert a fresh task row.
        async fn insert_task(&self, task: &Task) -> Result<(), CoreError>;

        /// Load a task by id.
        async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError>;

        /// Optimistic-concurrency status flip.
        async fn set_task_status_if_current(
            &self,
            id: &TaskId,
            expected: TaskStatus,
            status: TaskStatus,
            outcomes: Option<&[ExecutionTaskOutcome]>,
            terminal_payload: Option<&JsonObject>,
        ) -> Result<Option<Task>, CoreError>;

        /// Bulk-latch attempt task rows to [`TaskStatus::Cancelled`] before runtime
        /// teardown.
        async fn latch_attempt_tasks_cancelled(
            &self,
            attempt_id: &AttemptId,
            ids: &[TaskId],
        ) -> Result<(), CoreError>;

        /// All tasks owned by one request, ordered by creation.
        async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError>;
    }

    /// Persistence surface for top-level requests.
    #[async_trait]
    pub trait RequestStore: Sealed + Send + Sync {
        /// Create a new request row.
        async fn create_request(
            &self,
            request_id: &RequestId,
            cwd: &str,
            sandbox_id: Option<&SandboxId>,
            request_prompt: &str,
        ) -> Result<(), CoreError>;

        /// Load a request by id.
        async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError>;

        /// Set the root task id and return the updated request.
        async fn set_root_task_id(
            &self,
            id: &RequestId,
            root_task_id: &TaskId,
        ) -> Result<Request, CoreError>;

        /// Finish the request with `status`, stamping `finished_at` server-side.
        async fn finish_request(
            &self,
            id: &RequestId,
            status: RequestStatus,
        ) -> Result<Option<Request>, CoreError>;

        /// List all requests, newest first.
        async fn list(&self) -> Result<Vec<Request>, CoreError>;
    }
}
mod task_agent_run {
    //! Task-agent-run lineage persistence contract.

    use async_trait::async_trait;

    use crate::{
        AgentName, AgentRunId, AgentRunRecordIndex, CoreError, CreatedTaskAgentRun, JsonObject,
        ParentAgentRunAnchor, ParentedAgentRunKind, ParentedRun, RequestId, RunningRequestAgentRun,
        TaskExecutionIndex, TaskId, TaskRun, TaskStatus, ToolUseId, WorkflowCoordinates,
        WorkflowNodeId,
    };

    use super::Sealed;

    /// Persistence surface for the merged `task_runs` and `parented_runs` lineage.
    #[async_trait]
    pub trait TaskAgentRunStore: Sealed + Send + Sync {
        /// Create the root task-agent-run row and bind `Request.root_task_id`.
        async fn create_root_task_agent_run(
            &self,
            request_id: &RequestId,
            agent_run_id: &AgentRunId,
            agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError>;

        /// Create a workflow task-agent-run row.
        async fn create_workflow_task_agent_run(
            &self,
            request_id: &RequestId,
            agent_run_id: &AgentRunId,
            workflow: &WorkflowCoordinates,
            workflow_node_id: &WorkflowNodeId,
            agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError>;

        /// Create a parent-launched subagent/advisor row with a derived own task id.
        async fn create_parented_task_agent_run(
            &self,
            agent_run_id: &AgentRunId,
            parent: &ParentAgentRunAnchor,
            kind: ParentedAgentRunKind,
            tool_use_id: Option<&ToolUseId>,
            agent_name: &AgentName,
        ) -> Result<CreatedTaskAgentRun, CoreError>;

        /// Finish a root/workflow task-agent-run row.
        async fn finish_task_run(
            &self,
            agent_run_id: &AgentRunId,
            status: TaskStatus,
            terminal_payload: Option<&JsonObject>,
            token_count: i64,
            error: Option<&str>,
        ) -> Result<Option<TaskRun>, CoreError>;

        /// Finish a parent-launched subagent/advisor row.
        async fn finish_parented_run(
            &self,
            agent_run_id: &AgentRunId,
            status: TaskStatus,
            terminal_payload: Option<&JsonObject>,
            token_count: i64,
            error: Option<&str>,
        ) -> Result<Option<ParentedRun>, CoreError>;

        /// Resolve the record-index input for one run id.
        async fn record_index_for_agent_run(
            &self,
            agent_run_id: &AgentRunId,
        ) -> Result<Option<AgentRunRecordIndex>, CoreError>;

        /// Load one task-agent-run row by task id.
        async fn get_task_run(&self, task_id: &TaskId) -> Result<Option<TaskRun>, CoreError>;

        /// Load root/workflow task-agent-run rows for one request.
        async fn list_task_runs_for_request(
            &self,
            request_id: &RequestId,
        ) -> Result<Vec<TaskRun>, CoreError>;

        /// Load running agent runs for one request across root/workflow and
        /// parent-launched lineage rows.
        async fn list_running_agent_runs_for_request(
            &self,
            request_id: &RequestId,
        ) -> Result<Vec<RunningRequestAgentRun>, CoreError>;

        /// Load parent-launched child runs for one parent task and kind.
        async fn list_parented_runs_for_parent_task(
            &self,
            parent_task_id: &TaskId,
            kind: ParentedAgentRunKind,
        ) -> Result<Vec<ParentedRun>, CoreError>;

        /// Derive the flat read-side child index for one task.
        async fn task_execution_index(
            &self,
            task_id: &TaskId,
        ) -> Result<Option<TaskExecutionIndex>, CoreError>;
    }

    /// Build the deterministic root task id for a request.
    ///
    /// Root tasks are anchored by the request id, so the row-creation owner can bind
    /// `requests.root_task_id` without the spawn caller passing the new run's own
    /// task id back into the target contract.
    #[must_use]
    pub fn root_task_id(request_id: &RequestId) -> TaskId {
        format!("root-{request_id}")
            .parse()
            .expect("root-{request_id} is non-empty, so TaskId parsing cannot fail")
    }

    /// Build a deterministic workflow task-agent-run task id.
    ///
    /// Planner, generator, and reducer ids are derived from the attempt id plus the
    /// workflow-local id assigned for that role.
    ///
    /// # Errors
    /// Returns [`CoreError`] when the derived id violates the [`TaskId`] invariant.
    pub fn workflow_task_id(
        attempt_id: &crate::AttemptId,
        workflow_node_id: &WorkflowNodeId,
    ) -> Result<TaskId, CoreError> {
        let value = match workflow_node_id {
            WorkflowNodeId::Planner { planner_id } => {
                format!("{}:planner:{}", attempt_id.as_str(), planner_id.as_str())
            }
            WorkflowNodeId::Generator { generator_id } => {
                format!("{}:gen:{}", attempt_id.as_str(), generator_id.as_str())
            }
            WorkflowNodeId::Reducer { reducer_id } => {
                format!("{}:red:{}", attempt_id.as_str(), reducer_id.as_str())
            }
        };
        value.parse()
    }

    /// Build the deterministic parented-run task id from launch facts.
    ///
    /// # Errors
    /// Returns [`CoreError`] when `tool_use_id` is absent or the derived id is not a
    /// valid [`TaskId`].
    pub fn parented_task_id(
        parent_agent_run_id: &AgentRunId,
        kind: ParentedAgentRunKind,
        tool_use_id: Option<&ToolUseId>,
    ) -> Result<TaskId, CoreError> {
        let tool_use_id = tool_use_id.ok_or_else(|| {
            CoreError::Store("parented task-agent-run creation requires tool_use_id".to_owned())
        })?;
        let segment = match kind {
            ParentedAgentRunKind::Subagent => "sub",
            ParentedAgentRunKind::Advisor => "adv",
        };
        format!(
            "{}:{segment}:{}",
            parent_agent_run_id.as_str(),
            tool_use_id.as_str()
        )
        .parse()
    }
}
mod workflow {
    //! Workflow-facing persistence contracts.

    use async_trait::async_trait;

    use crate::{
        AgentRunId, Attempt, AttemptBudget, AttemptClosure, AttemptId, CoreError, DeferredGoal,
        Iteration, IterationCreationReason, IterationId, IterationStatus, MaterializedPlan,
        RequestId, TaskId, ToolUseId, UtcDateTime, Workflow, WorkflowId, WorkflowStatus,
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

        /// Mark all open workflows for a request as cancelled.
        async fn cancel_open_workflows_for_request(
            &self,
            request_id: &RequestId,
            reason: &str,
        ) -> Result<usize, CoreError>;
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

        /// Mark all open iterations for a request as cancelled.
        async fn cancel_open_iterations_for_request(
            &self,
            request_id: &RequestId,
            reason: &str,
        ) -> Result<usize, CoreError>;
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
        async fn close(
            &self,
            id: &AttemptId,
            closure: AttemptClosure,
        ) -> Result<Attempt, CoreError>;

        /// All attempts of an iteration, ordered by attempt sequence number.
        async fn list_for_iteration(
            &self,
            iteration_id: &IterationId,
        ) -> Result<Vec<Attempt>, CoreError>;

        /// Mark all open attempts for a request as cancelled.
        async fn cancel_open_attempts_for_request(
            &self,
            request_id: &RequestId,
        ) -> Result<usize, CoreError>;
    }
}

pub use engine::AgentRunStore;
pub use model_registry::ModelStore;
pub use request_task::{RequestStore, TaskStore};
pub use task_agent_run::{parented_task_id, root_task_id, workflow_task_id, TaskAgentRunStore};
pub use workflow::{AttemptStore, IterationStore, WorkflowStore};

/// Alias for the error every store method returns.
pub type StoreError = crate::CoreError;

/// Sealing marker for the store traits.
///
/// Implemented by workspace repository types and in-crate test fakes only.
#[doc(hidden)]
pub trait Sealed {}
