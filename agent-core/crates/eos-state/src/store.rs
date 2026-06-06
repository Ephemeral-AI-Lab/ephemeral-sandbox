//! The seven per-entity async `Store` traits (ISP — no god-store).
//!
//! Ports `workflow/_core/persistence.py` (`*StoreProtocol`s) plus the promoted
//! `AgentRunStore`/`ModelStore`. Each trait lists only the methods agent-core
//! calls. Concrete sqlx implementations live in `eos-db`; in-crate `#[cfg(test)]`
//! fakes prove substitutability.
//!
//! Object safety: every trait is used behind `Arc<dyn Trait>` in the
//! `eos-runtime` composition root, so they carry `#[async_trait]` (native
//! async-fn-in-trait is not yet `dyn`-safe; anchor §6). `StoreError` is an alias
//! for `eos-types::CoreError` (spec §8 — no crate-local error enum).

use async_trait::async_trait;

use eos_types::{
    AgentRunId, AttemptId, CoreError, IterationId, JsonObject, RequestId, SandboxId, TaskId,
    UtcDateTime, WorkflowId,
};

use crate::agent_run::AgentRun;
use crate::attempt::{Attempt, AttemptClosure};
use crate::iteration::{Iteration, IterationCreationReason, IterationStatus};
use crate::model::ModelRegistration;
use crate::outcomes::ExecutionTaskOutcome;
use crate::pagination::{Page, PageResult, RequestListFilter};
use crate::plan::{AttemptBudget, DeferredGoal, MaterializedPlan};
use crate::request::{Request, RequestStatus};
use crate::task::{Task, TaskStatus};
use crate::workflow::{Workflow, WorkflowStatus};

/// Alias for the error every `Store` method returns (anchor §5/§8).
pub type StoreError = CoreError;

/// Sealing marker for the `Store` traits.
///
/// Implemented by the `eos-db` sqlx repositories and in-crate test fakes only.
/// It is `#[doc(hidden)]` rather than fully private because the implementing
/// repositories live in a separate downstream crate (a strictly-private marker
/// would be unreachable to them); external crates outside this workspace should
/// not implement the `Store` traits (`api-sealed-trait`).
#[doc(hidden)]
pub trait Sealed {}

/// Persistence surface for [`Workflow`] (Python `WorkflowStoreProtocol`).
#[async_trait]
pub trait WorkflowStore: Sealed + Send + Sync {
    /// Insert a fresh open workflow and return it.
    async fn insert(
        &self,
        request_id: &RequestId,
        parent_task_id: &TaskId,
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

    /// Set status (and optionally `closed_at`/`outcomes`). `None` outcomes leaves
    /// the persisted projection unchanged.
    async fn set_status(
        &self,
        id: &WorkflowId,
        status: WorkflowStatus,
        closed_at: Option<UtcDateTime>,
        outcomes: Option<&str>,
    ) -> Result<Workflow, CoreError>;

    /// All workflows launched by one parent task, ordered by `created_at`.
    async fn list_for_parent_task(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<Vec<Workflow>, CoreError>;
}

/// Persistence surface for request/task (Python `TaskStoreProtocol`, task half).
#[async_trait]
pub trait TaskStore: Sealed + Send + Sync {
    /// Insert when absent, full-field update when present; bumps `updated_at`.
    async fn upsert_task(&self, task: &Task) -> Result<(), CoreError>;

    /// Load a task by id.
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError>;

    /// Set status (and optionally outcomes/terminal result). `None` for an
    /// optional param leaves that column unchanged.
    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, CoreError>;

    /// Optimistic-concurrency status flip (Python `set_task_status_if_current`).
    /// `Ok(None)` ⇒ the current status did not match `expected`.
    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError>;

    /// All tasks owned by one request, ordered by creation — the request task
    /// tree (`needs` edges live on each [`Task`]). Read-side API for the backend
    /// composition root (spec §State Reader).
    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError>;
}

/// Persistence surface for [`Iteration`] (Python `IterationStoreProtocol`).
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

    /// Set status (and optionally `closed_at`/`outcomes`). `None` outcomes leaves
    /// the persisted projection unchanged.
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

    /// Atomically transition to `succeeded` and write the canonical outcomes.
    async fn close_succeeded(
        &self,
        id: &IterationId,
        outcomes: &str,
        closed_at: Option<UtcDateTime>,
    ) -> Result<Iteration, CoreError>;

    /// All iterations of a workflow, ordered by `sequence_no`.
    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<Iteration>, CoreError>;
}

/// Persistence surface for [`Attempt`] (Python `AttemptStoreProtocol`).
#[async_trait]
pub trait AttemptStore: Sealed + Send + Sync {
    /// Insert a fresh attempt (`stage=plan`, `status=running`) and return it.
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

    /// Record a materialized planner DAG and transition the attempt to RUN.
    async fn record_plan(
        &self,
        id: &AttemptId,
        plan: &MaterializedPlan,
    ) -> Result<Attempt, CoreError>;

    /// Close the attempt with a typed terminal closure.
    async fn close(&self, id: &AttemptId, closure: AttemptClosure) -> Result<Attempt, CoreError>;

    /// All attempts of an iteration, ordered by `attempt_sequence_no`.
    async fn list_for_iteration(
        &self,
        iteration_id: &IterationId,
    ) -> Result<Vec<Attempt>, CoreError>;
}

/// Persistence surface for requests (Python `TaskStoreProtocol`, request half,
/// split out per ISP).
#[async_trait]
pub trait RequestStore: Sealed + Send + Sync {
    /// Create a new request row (status defaults to `running`).
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
    /// Idempotent on an already-terminal request (returns it unchanged).
    /// `Ok(None)` ⇒ the request does not exist.
    async fn finish_request(
        &self,
        id: &RequestId,
        status: RequestStatus,
    ) -> Result<Option<Request>, CoreError>;

    /// List requests matching `filter`, newest first, within the `page` window.
    /// `total` counts every match ignoring the window. Read-side API for the
    /// backend composition root (spec §State Reader); not used by agent-core's
    /// own request lifecycle.
    async fn list(
        &self,
        filter: RequestListFilter,
        page: Page,
    ) -> Result<PageResult<Request>, CoreError>;
}

/// Persistence surface for [`AgentRun`] (Python `AgentRunStore`).
#[async_trait]
pub trait AgentRunStore: Sealed + Send + Sync {
    /// Create a run row with only the create-time fields set.
    async fn create_run(
        &self,
        agent_run_id: &AgentRunId,
        task_id: &TaskId,
        agent_name: &str,
        initial_messages: Option<&[JsonObject]>,
    ) -> Result<AgentRun, CoreError>;

    /// Write the finish-time fields. `Ok(None)` ⇒ the run does not exist.
    async fn finish_run(
        &self,
        agent_run_id: &AgentRunId,
        message_history: Option<&[JsonObject]>,
        terminal_tool_result: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<AgentRun>, CoreError>;

    /// Load a run by id.
    async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError>;

    /// The latest agent run for one task, if any. `AgentRun.task_id` is 1:1 in
    /// practice; the newest row wins if a task was ever re-run. Read-side API for
    /// the backend composition root (spec §State Reader).
    async fn get_for_task(&self, task_id: &TaskId) -> Result<Option<AgentRun>, CoreError>;
}

/// Persistence surface for [`ModelRegistration`] (Python `ModelStore`).
#[async_trait]
pub trait ModelStore: Sealed + Send + Sync {
    /// Create or update a registration. `kwargs` is serialized to `kwargs_json`
    /// by `eos-db`; `activate` deactivates all others first.
    async fn register(
        &self,
        model_key: &str,
        label: &str,
        class_path: &str,
        kwargs: &JsonObject,
        activate: bool,
    ) -> Result<ModelRegistration, CoreError>;

    /// Delete by key; `Ok(false)` ⇒ no such key.
    async fn delete(&self, model_key: &str) -> Result<bool, CoreError>;

    /// Load a registration by key.
    async fn get(&self, model_key: &str) -> Result<Option<ModelRegistration>, CoreError>;

    /// The single active registration, if any.
    async fn active(&self) -> Result<Option<ModelRegistration>, CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::FakeTaskStore;
    use crate::task::{Task, TaskRole, TaskStatus};
    use eos_types::RequestId;

    fn sample_task() -> Task {
        Task {
            id: "t1".parse().expect("id"),
            request_id: RequestId::new_v4(),
            role: TaskRole::Generator,
            instruction: "do the thing".to_owned(),
            status: TaskStatus::Pending,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: Some("coder".to_owned()),
            needs: Vec::new(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        }
    }

    // AC-eos-state-06: TaskStore is fully typed — a trait-object round-trips a
    // Task losslessly (no dict/Value row type anywhere in the surface).
    #[tokio::test]
    async fn task_store_is_typed() {
        let store: &dyn TaskStore = &FakeTaskStore::new();
        let task = sample_task();
        store.upsert_task(&task).await.expect("upsert");
        let got = store.get(&task.id).await.expect("get").expect("present");
        assert_eq!(got, task);
    }

    // AC-eos-state-08: set_task_status_if_current returns Ok(None) on mismatch
    // and Ok(Some(_)) on a successful flip.
    #[tokio::test]
    async fn optimistic_status_flip() {
        let store = FakeTaskStore::new();
        let task = sample_task(); // status = Pending
        store.upsert_task(&task).await.expect("upsert");

        // Mismatched expectation is a no-op.
        let miss = store
            .set_task_status_if_current(&task.id, TaskStatus::Running, TaskStatus::Done, None, None)
            .await
            .expect("cas");
        assert!(miss.is_none());
        assert_eq!(
            store
                .get(&task.id)
                .await
                .expect("get")
                .expect("present")
                .status,
            TaskStatus::Pending
        );

        // Matching expectation flips and returns the updated task.
        let hit = store
            .set_task_status_if_current(
                &task.id,
                TaskStatus::Pending,
                TaskStatus::Running,
                None,
                None,
            )
            .await
            .expect("cas")
            .expect("flipped");
        assert_eq!(hit.status, TaskStatus::Running);
    }
}
