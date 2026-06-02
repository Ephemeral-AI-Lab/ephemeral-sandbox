use std::sync::Arc;

use eos_state::{AttemptId, TaskId, TaskStatus, WorkflowStatus};

use crate::attempt::AttemptDeps;
use crate::iteration::OpenIterationCoordinatorRegistry;
use crate::lifecycle::WorkflowLifecycle;
use crate::{Result, WorkflowError};

/// Workflow start result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedWorkflow {
    /// Launching task.
    pub parent_task_id: TaskId,
    /// Parent attempt, if any.
    pub parent_attempt_id: Option<AttemptId>,
    /// Created workflow id.
    pub workflow_id: eos_state::WorkflowId,
    /// Created iteration id.
    pub iteration_id: eos_state::IterationId,
    /// Created first attempt id.
    pub attempt_id: AttemptId,
}

/// Single safe entry point from a running task to a delegated workflow.
#[derive(Clone)]
pub struct WorkflowStarter {
    deps: AttemptDeps,
}

impl std::fmt::Debug for WorkflowStarter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowStarter").finish_non_exhaustive()
    }
}

impl WorkflowStarter {
    /// Create a starter.
    #[must_use]
    pub fn new(deps: AttemptDeps) -> Self {
        Self { deps }
    }

    /// Start a delegated workflow from `parent_task_id`.
    ///
    /// # Errors
    /// Returns [`WorkflowError`] when the parent is not a running task or row
    /// creation/start fails.
    pub async fn start(&self, prompt: &str, parent_task_id: &TaskId) -> Result<StartedWorkflow> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(WorkflowError::BlankPrompt);
        }
        let parent = self
            .assert_parent_running_and_no_open_child(parent_task_id)
            .await?;
        let request_id = parent.request_id.clone();
        let parent_attempt_id = parent.attempt_id.clone();
        let iteration_coordinators = self.deps.iteration_coordinators.clone().ok_or_else(|| {
            WorkflowError::invariant("workflow starter requires open iteration coordinators")
        })?;
        let lifecycle = WorkflowLifecycle::new(self.deps.clone(), iteration_coordinators);
        let workflow = lifecycle
            .create_workflow(&request_id, parent_task_id, prompt)
            .await?;
        let (iteration, coordinator) = lifecycle
            .create_iteration_with_coordinator(&workflow.id)
            .await?;
        let attempt = match coordinator.create_and_start_first_attempt().await {
            Ok(attempt) => attempt,
            Err(err) => {
                self.compensate_failed_start(&workflow.id, &iteration.id)
                    .await?;
                return Err(err);
            }
        };
        Ok(StartedWorkflow {
            parent_task_id: parent_task_id.clone(),
            parent_attempt_id,
            workflow_id: workflow.id,
            iteration_id: iteration.id,
            attempt_id: attempt.id,
        })
    }

    async fn assert_parent_running_and_no_open_child(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<eos_state::Task> {
        let task = self
            .deps
            .task_store
            .get(parent_task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("task", parent_task_id.as_str()))?;
        if task.status != TaskStatus::Running {
            return Err(WorkflowError::invariant(format!(
                "task {:?} is not running; delegated workflow start requires a running parent task",
                parent_task_id.as_str()
            )));
        }
        let open = self
            .deps
            .workflow_store
            .list_for_parent_task(parent_task_id)
            .await?
            .into_iter()
            .find(eos_state::Workflow::is_open);
        if let Some(workflow) = open {
            return Err(WorkflowError::invariant(format!(
                "task {:?} already has an open delegated workflow {:?}",
                parent_task_id.as_str(),
                workflow.id.as_str()
            )));
        }
        Ok(task)
    }

    async fn compensate_failed_start(
        &self,
        workflow_id: &eos_state::WorkflowId,
        iteration_id: &eos_state::IterationId,
    ) -> Result<()> {
        if let Some(iteration) = self.deps.iteration_store.get(iteration_id).await? {
            if let Some(attempt_id) = iteration.latest_attempt_id() {
                if let Some(attempt) = self.deps.attempt_store.get(attempt_id).await? {
                    if !attempt.is_closed() {
                        self.deps
                            .attempt_store
                            .close(
                                attempt_id,
                                eos_state::AttemptStatus::Failed,
                                Some(eos_state::AttemptFailReason::StartupFailed),
                                None,
                                eos_state::UtcDateTime::now(),
                            )
                            .await?;
                    }
                }
            }
        }
        self.deps
            .iteration_store
            .set_status(
                iteration_id,
                eos_state::IterationStatus::Cancelled,
                Some(eos_state::UtcDateTime::now()),
                None,
            )
            .await?;
        self.deps
            .workflow_store
            .set_status(
                workflow_id,
                WorkflowStatus::Cancelled,
                Some(eos_state::UtcDateTime::now()),
                None,
            )
            .await?;
        if let Some(registry) = &self.deps.iteration_coordinators {
            registry.deregister(iteration_id);
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn _assert_arc(_: Arc<OpenIterationCoordinatorRegistry>) {}
