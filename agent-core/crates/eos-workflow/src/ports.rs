use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{
    execution_outcome_for_submission, AttemptFailReason, AttemptStatus, ExecutionRole,
    GeneratorSubmission, IterationStatus, ReducerSubmission, Task, TaskOutcomeStatus, TaskRole,
    TaskStatus, TaskStore, WorkflowId, WorkflowStatus,
};
use eos_tools::{
    OutstandingWorkflow, PlanSubmissionPort, PlannerPlan, SubmissionAck, ToolError,
    WorkflowControlPort,
};
use eos_types::WorkflowSessionId;
use parking_lot::Mutex;
use serde_json::json;

use crate::attempt::AttemptOrchestratorRegistry;
use crate::util::json_object;
use crate::{WorkflowError, WorkflowStarter};

/// Recording adapter from the `eos-tools` planner/generator/reducer terminal
/// ports to the active per-attempt orchestrators (Path A-recording).
///
/// The submit tool writes the agent's real submission straight to the
/// orchestrator's non-advancing `record_*` variants and returns the
/// orchestrator's real ack; advancing the DAG stays the exclusive job of the
/// single `advance_run_stage` loop (D4: exactly one writer). This is the wired
/// implementor of [`PlanSubmissionPort`], constructed once at the composition
/// root over the shared attempt registry.
#[derive(Clone)]
pub struct PlanSubmissionAdapter {
    registry: Arc<AttemptOrchestratorRegistry>,
}

impl std::fmt::Debug for PlanSubmissionAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanSubmissionAdapter")
            .finish_non_exhaustive()
    }
}

impl PlanSubmissionAdapter {
    /// Create a submission adapter over the active attempt registry.
    #[must_use]
    pub fn new(registry: Arc<AttemptOrchestratorRegistry>) -> Self {
        Self { registry }
    }
}

impl eos_tools::ports::Sealed for PlanSubmissionAdapter {}

#[async_trait]
impl PlanSubmissionPort for PlanSubmissionAdapter {
    async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, ToolError> {
        let Some(orchestrator) = self.registry.get(&plan.attempt_id) else {
            return Ok(SubmissionAck::Rejected(format!(
                "attempt {:?} is not active",
                plan.attempt_id.as_str()
            )));
        };
        submission_ack(orchestrator.record_plan(plan).await)
    }

    async fn submit_generator(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<SubmissionAck, ToolError> {
        let Some(orchestrator) = self.registry.get(&submission.attempt_id) else {
            return Ok(SubmissionAck::Rejected(format!(
                "attempt {:?} is not active",
                submission.attempt_id.as_str()
            )));
        };
        submission_ack(orchestrator.record_generator_submission(submission).await)
    }

    async fn apply_reducer(
        &self,
        submission: ReducerSubmission,
    ) -> Result<SubmissionAck, ToolError> {
        let Some(orchestrator) = self.registry.get(&submission.attempt_id) else {
            return Ok(SubmissionAck::Rejected(format!(
                "attempt {:?} is not active",
                submission.attempt_id.as_str()
            )));
        };
        submission_ack(orchestrator.record_reducer_submission(submission).await)
    }
}

fn submission_ack(result: crate::Result<()>) -> Result<SubmissionAck, ToolError> {
    match result {
        Ok(()) => Ok(SubmissionAck::Accepted),
        Err(WorkflowError::Store(err)) => Err(ToolError::Store(err)),
        Err(WorkflowError::Tool(err)) => Err(err),
        Err(WorkflowError::Join(err)) => Err(ToolError::Internal(err)),
        Err(err) => Ok(SubmissionAck::Rejected(err.to_string())),
    }
}

/// Adapter from `eos-tools` workflow-control ports to delegated workflow state.
#[derive(Clone)]
pub struct WorkflowControlAdapter {
    starter: WorkflowStarter,
    workflow_store: Arc<dyn eos_state::WorkflowStore>,
    iteration_store: Arc<dyn eos_state::IterationStore>,
    attempt_store: Arc<dyn eos_state::AttemptStore>,
    task_store: Arc<dyn TaskStore>,
    handles: Arc<WorkflowHandleRegistry>,
}

impl std::fmt::Debug for WorkflowControlAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowControlAdapter")
            .finish_non_exhaustive()
    }
}

impl WorkflowControlAdapter {
    /// Create a workflow-control adapter.
    #[must_use]
    pub fn new(
        starter: WorkflowStarter,
        workflow_store: Arc<dyn eos_state::WorkflowStore>,
        iteration_store: Arc<dyn eos_state::IterationStore>,
        attempt_store: Arc<dyn eos_state::AttemptStore>,
        task_store: Arc<dyn TaskStore>,
    ) -> Self {
        Self {
            starter,
            workflow_store,
            iteration_store,
            attempt_store,
            task_store,
            handles: Arc::new(WorkflowHandleRegistry::default()),
        }
    }
}

impl eos_tools::ports::Sealed for WorkflowControlAdapter {}

#[async_trait]
impl WorkflowControlPort for WorkflowControlAdapter {
    async fn start(
        &self,
        parent_task_id: &eos_state::TaskId,
        _agent_id: &str,
        workflow_goal: &str,
    ) -> Result<eos_tools::StartedWorkflow, ToolError> {
        let started = self
            .starter
            .start(workflow_goal, parent_task_id)
            .await
            .map_err(workflow_control_error)?;
        let workflow_task_id = self.handles.handle_for_workflow(&started.workflow_id)?;
        Ok(eos_tools::StartedWorkflow {
            workflow_task_id,
            workflow_id: started.workflow_id,
        })
    }

    async fn status(
        &self,
        workflow_id: &WorkflowId,
        workflow_task_id: Option<&WorkflowSessionId>,
    ) -> Result<String, ToolError> {
        if let Some(handle) = workflow_task_id {
            let Some(handle_workflow_id) = self.handles.workflow_id_for_handle(handle) else {
                return Ok(format!("Workflow handle {handle} was not found."));
            };
            if &handle_workflow_id != workflow_id {
                return Ok(format!(
                    "Workflow handle {handle} does not refer to workflow {workflow_id}."
                ));
            }
        }
        let Some(workflow) = self.workflow_store.get(workflow_id).await? else {
            return Ok(format!("Workflow {workflow_id} was not found."));
        };
        let handle = self.handles.handle_for_workflow(&workflow.id)?;
        let mut text = format!(
            "Workflow {} ({}) is {:?}. Goal: {}",
            workflow.id, handle, workflow.status, workflow.workflow_goal
        );
        if let Some(outcomes) = &workflow.outcomes {
            text.push_str("\nOutcomes:\n");
            text.push_str(outcomes);
        }
        Ok(text)
    }

    async fn cancel(
        &self,
        workflow_task_id: &WorkflowSessionId,
        reason: &str,
    ) -> Result<String, ToolError> {
        let Some(workflow_id) = self.handles.workflow_id_for_handle(workflow_task_id) else {
            return Ok(format!("Workflow handle {workflow_task_id} was not found."));
        };
        let Some(workflow) = self.workflow_store.get(&workflow_id).await? else {
            return Ok(format!("Workflow handle {workflow_task_id} was not found."));
        };
        if workflow.status != WorkflowStatus::Open {
            return Ok(format!(
                "Workflow {workflow_id} is already {:?}.",
                workflow.status
            ));
        }
        self.cancel_workflow_state(&workflow, reason).await?;
        Ok(format!("Workflow {workflow_id} cancelled: {reason}"))
    }

    async fn find_outstanding(
        &self,
        parent_task_id: &eos_state::TaskId,
        _agent_id: &str,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
        self.workflow_store
            .list_for_parent_task(parent_task_id)
            .await?
            .into_iter()
            .filter(eos_state::Workflow::is_open)
            .map(|workflow| {
                Ok(OutstandingWorkflow {
                    workflow_task_id: self.handles.handle_for_workflow(&workflow.id)?,
                    workflow_id: workflow.id,
                    workflow_goal: workflow.workflow_goal,
                })
            })
            .collect()
    }

    async fn is_nested_workflow(&self, workflow_id: &WorkflowId) -> Result<bool, ToolError> {
        let Some(workflow) = self.workflow_store.get(workflow_id).await? else {
            return Ok(false);
        };
        let Some(parent) = self.task_store.get(&workflow.parent_task_id).await? else {
            return Ok(false);
        };
        Ok(parent.workflow_id.is_some())
    }
}

impl WorkflowControlAdapter {
    async fn cancel_workflow_state(
        &self,
        workflow: &eos_state::Workflow,
        reason: &str,
    ) -> Result<(), ToolError> {
        let now = eos_state::UtcDateTime::now();
        let outcome_text = if reason.trim().is_empty() {
            "Delegated workflow was cancelled."
        } else {
            reason
        };
        let outcomes = json!([{
            "status": "failed",
            "role": "workflow",
            "task_id": workflow.id.as_str(),
            "outcome": outcome_text,
        }])
        .to_string();

        for iteration in self.iteration_store.list_for_workflow(&workflow.id).await? {
            if !iteration.is_open() {
                continue;
            }
            for attempt in self.attempt_store.list_for_iteration(&iteration.id).await? {
                if attempt.is_closed() {
                    continue;
                }
                for task_id in attempt
                    .planner_task_id
                    .iter()
                    .chain(attempt.generator_task_ids.iter())
                    .chain(attempt.reducer_task_ids.iter())
                {
                    if let Some(task) = self.task_store.get(task_id).await? {
                        self.cancel_active_task(&task, outcome_text).await?;
                    }
                }
                self.attempt_store
                    .close(
                        &attempt.id,
                        AttemptStatus::Failed,
                        Some(AttemptFailReason::TaskFailed),
                        Some(&[]),
                        now,
                    )
                    .await?;
            }
            self.iteration_store
                .set_status(
                    &iteration.id,
                    IterationStatus::Cancelled,
                    Some(now),
                    Some(&outcomes),
                )
                .await?;
        }
        self.workflow_store
            .set_status(
                &workflow.id,
                WorkflowStatus::Cancelled,
                Some(now),
                Some(&outcomes),
            )
            .await?;
        Ok(())
    }

    async fn cancel_active_task(&self, task: &Task, outcome_text: &str) -> Result<(), ToolError> {
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return Ok(());
        }
        let terminal = json_object("fail_reason", "workflow_cancelled");
        let outcomes = cancellation_outcomes(task, outcome_text);
        self.task_store
            .set_task_status_if_current(
                &task.id,
                task.status,
                TaskStatus::Failed,
                Some(&outcomes),
                Some(&terminal),
            )
            .await?;
        Ok(())
    }
}

fn workflow_control_error(err: WorkflowError) -> ToolError {
    match err {
        WorkflowError::Store(err) => ToolError::Store(err),
        WorkflowError::Tool(err) => err,
        other => ToolError::Internal(other.to_string()),
    }
}

#[derive(Debug, Default)]
struct WorkflowHandleRegistry {
    next_handle: AtomicU64,
    inner: Mutex<WorkflowHandleMaps>,
}

#[derive(Debug, Default)]
struct WorkflowHandleMaps {
    workflow_by_handle: HashMap<WorkflowSessionId, WorkflowId>,
    handle_by_workflow: HashMap<WorkflowId, WorkflowSessionId>,
}

impl WorkflowHandleRegistry {
    fn handle_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<WorkflowSessionId, ToolError> {
        let mut guard = self.inner.lock();
        if let Some(handle) = guard.handle_by_workflow.get(workflow_id) {
            return Ok(handle.clone());
        }
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed) + 1;
        let handle: WorkflowSessionId = format!("wf_{id}").parse()?;
        guard
            .workflow_by_handle
            .insert(handle.clone(), workflow_id.clone());
        guard
            .handle_by_workflow
            .insert(workflow_id.clone(), handle.clone());
        Ok(handle)
    }

    fn workflow_id_for_handle(&self, handle: &WorkflowSessionId) -> Option<WorkflowId> {
        self.inner.lock().workflow_by_handle.get(handle).cloned()
    }
}

fn cancellation_outcomes(task: &Task, outcome_text: &str) -> Vec<eos_state::ExecutionTaskOutcome> {
    let role = match task.role {
        TaskRole::Generator => ExecutionRole::Generator,
        TaskRole::Reducer => ExecutionRole::Reducer,
        TaskRole::Root | TaskRole::Planner => return Vec::new(),
    };
    vec![execution_outcome_for_submission(
        task.id.clone(),
        role,
        TaskOutcomeStatus::Failed,
        outcome_text.to_owned(),
    )]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use eos_state::{
        AttemptFailReason, AttemptStatus, IterationStatus, TaskStatus, WorkflowStatus,
    };
    use eos_tools::WorkflowControlPort as _;
    use serde_json::json;

    use super::*;
    use crate::testsupport::{root_task, MemoryStores, QueueRunner};

    // The workflow-control adapter mints `wf_<n>` handles (not workflow ids),
    // rejects a fabricated handle, and `cancel` tears down the delegated tree
    // (workflow + iteration CANCELLED, attempt FAILED, active tasks FAILED with
    // a `workflow_cancelled` marker) without mutating the parent.
    #[tokio::test]
    async fn workflow_control_uses_runtime_handles_and_cancels_child_state() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let deps = stores.deps(runner);
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let adapter = WorkflowControlAdapter::new(
            WorkflowStarter::new(deps),
            stores.clone(),
            stores.clone(),
            stores.clone(),
            stores.clone(),
        );

        let started = adapter
            .start(&parent.id, "agent-1", "delegated goal")
            .await
            .unwrap();
        assert_eq!(started.workflow_task_id.as_str(), "wf_1");
        let derived_handle: eos_types::WorkflowSessionId =
            format!("wf_{}", started.workflow_id.as_str())
                .parse()
                .unwrap();
        assert!(adapter
            .status(&started.workflow_id, Some(&derived_handle))
            .await
            .unwrap()
            .contains("was not found"));

        adapter
            .cancel(&started.workflow_task_id, "stop now")
            .await
            .unwrap();

        let workflow = stores.workflow(&started.workflow_id).unwrap();
        assert_eq!(workflow.status, WorkflowStatus::Cancelled);
        let iteration_id = workflow.iteration_ids.first().unwrap();
        let iteration = stores.iteration(iteration_id).unwrap();
        assert_eq!(iteration.status, IterationStatus::Cancelled);
        let attempt_id = iteration.attempt_ids.first().unwrap();
        let attempt = stores.attempt(attempt_id).unwrap();
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(attempt.fail_reason, Some(AttemptFailReason::TaskFailed));
        let planner_task = stores
            .task(attempt.planner_task_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(planner_task.status, TaskStatus::Failed);
        assert_eq!(
            planner_task
                .terminal_tool_result
                .unwrap()
                .get("fail_reason"),
            Some(&json!("workflow_cancelled"))
        );
        assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
    }
}
