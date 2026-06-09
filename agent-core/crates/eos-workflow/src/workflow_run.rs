use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{
    AgentRunId, IterationCreationReason, IterationStatus, OpenDelegatedWorkflow,
    StartWorkflowRequest, StartedWorkflow, TaskId, TaskStatus, TerminalWorkflow, ToolUseId,
    Workflow, WorkflowApi, WorkflowApiError, WorkflowId, WorkflowStatus, WorkflowTerminalStatus,
};

use crate::attempt::{AttemptResources, OpenIterationCoordinatorRegistry};
use crate::iteration_run::{
    IterationClosedCallback, IterationRunClosed, IterationRunCoordinator, IterationRunOutcome,
};
use crate::{Result, WorkflowError};

type IterationCoordinatorFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<(eos_types::Iteration, Arc<IterationRunCoordinator>)>>
            + Send
            + 'a,
    >,
>;

/// Rich result returned by the in-crate workflow runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedWorkflowRun {
    /// Launching task.
    pub parent_task_id: TaskId,
    /// Created workflow id.
    pub workflow_id: WorkflowId,
    /// Created iteration id.
    pub iteration_id: eos_types::IterationId,
    /// Created first attempt id.
    pub attempt_id: eos_types::AttemptId,
    /// Delegated workflow goal.
    pub workflow_goal: String,
}

/// Single workflow lifecycle entry point.
#[derive(Clone)]
pub struct WorkflowRun {
    deps: AttemptResources,
    iteration_coordinators: Arc<OpenIterationCoordinatorRegistry>,
}

impl std::fmt::Debug for WorkflowRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRun").finish_non_exhaustive()
    }
}

impl WorkflowRun {
    /// Create a workflow runner.
    #[must_use]
    pub fn new(
        deps: AttemptResources,
        coordinators: Arc<OpenIterationCoordinatorRegistry>,
    ) -> Self {
        Self {
            deps: deps.with_iteration_coordinators(coordinators.clone()),
            iteration_coordinators: coordinators,
        }
    }

    /// Start a delegated workflow from `parent_task_id`.
    pub async fn start(
        &self,
        workflow_goal: &str,
        parent_task_id: &TaskId,
        parent_agent_run_id: &AgentRunId,
        tool_use_id: Option<&ToolUseId>,
    ) -> Result<StartedWorkflowRun> {
        let workflow_goal = workflow_goal.trim();
        if workflow_goal.is_empty() {
            return Err(WorkflowError::BlankPrompt);
        }
        let parent = self
            .assert_parent_running_and_no_open_child(parent_task_id, parent_agent_run_id)
            .await?;
        let workflow = self
            .deps
            .workflow_store
            .insert(
                &parent.request_id,
                parent_task_id,
                parent_agent_run_id,
                tool_use_id,
                workflow_goal,
            )
            .await?;
        let (iteration, coordinator) = self
            .create_iteration_with_coordinator(
                &workflow.id,
                IterationCreationReason::Initial,
                workflow_goal,
            )
            .await?;
        let attempt = match coordinator.create_and_start_first_attempt().await {
            Ok(attempt) => attempt,
            Err(err) => {
                self.compensate_failed_start(&workflow.id, &iteration.id)
                    .await?;
                return Err(err);
            }
        };
        Ok(StartedWorkflowRun {
            parent_task_id: parent_task_id.clone(),
            workflow_id: workflow.id,
            iteration_id: iteration.id,
            attempt_id: attempt.id,
            workflow_goal: workflow_goal.to_owned(),
        })
    }

    async fn assert_parent_running_and_no_open_child(
        &self,
        parent_task_id: &TaskId,
        parent_agent_run_id: &AgentRunId,
    ) -> Result<eos_types::Task> {
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
            .list_for_launching_agent_run(parent_agent_run_id)
            .await?
            .into_iter()
            .find(eos_types::Workflow::is_open);
        if let Some(workflow) = open {
            return Err(WorkflowError::invariant(format!(
                "task {:?} already has an open delegated workflow {:?}",
                parent_task_id.as_str(),
                workflow.id.as_str()
            )));
        }
        Ok(task)
    }

    fn create_iteration_with_coordinator<'a>(
        &'a self,
        workflow_id: &'a WorkflowId,
        reason: IterationCreationReason,
        iteration_goal: &'a str,
    ) -> IterationCoordinatorFuture<'a> {
        Box::pin(self.create_iteration_with_coordinator_inner(workflow_id, reason, iteration_goal))
    }

    async fn create_iteration_with_coordinator_inner(
        &self,
        workflow_id: &WorkflowId,
        reason: IterationCreationReason,
        iteration_goal: &str,
    ) -> Result<(eos_types::Iteration, Arc<IterationRunCoordinator>)> {
        let workflow = self.require_workflow(workflow_id).await?;
        if !workflow.is_open() {
            return Err(WorkflowError::invariant(format!(
                "workflow {:?} is not open",
                workflow.id.as_str()
            )));
        }
        let sequence_no = workflow.iteration_ids.len() as i64 + 1;
        let iteration = self
            .deps
            .iteration_store
            .insert(
                &workflow.id,
                sequence_no,
                reason,
                &workflow.workflow_goal,
                iteration_goal,
                self.deps.lifecycle_config.default_attempt_budget,
            )
            .await?;
        self.deps
            .workflow_store
            .append_iteration_id(&workflow.id, &iteration.id)
            .await?;

        let workflow_run = self.clone();
        let callback: IterationClosedCallback = Arc::new(move |closed: IterationRunClosed| {
            let workflow_run = workflow_run.clone();
            Box::pin(async move { workflow_run.handle_iteration_closed(closed).await })
        });
        let coordinator =
            IterationRunCoordinator::new(iteration.id.clone(), self.deps.clone(), callback);
        self.iteration_coordinators.register(coordinator.clone())?;
        Ok((iteration, coordinator))
    }

    pub(crate) async fn handle_iteration_closed(&self, closed: IterationRunClosed) -> Result<()> {
        let iteration = self
            .deps
            .iteration_store
            .get(&closed.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", closed.iteration_id.as_str()))?;
        self.iteration_coordinators.deregister(&iteration.id);

        match closed.outcome {
            IterationRunOutcome::Continue(deferred_goal) => {
                self.start_iteration_with_deferred_goal(
                    &iteration.workflow_id,
                    deferred_goal.as_str(),
                )
                .await
            }
            IterationRunOutcome::Complete => self
                .close_workflow(&iteration.workflow_id, WorkflowStatus::Succeeded)
                .await
                .map(|_| ()),
            IterationRunOutcome::Failed => self
                .close_workflow(&iteration.workflow_id, WorkflowStatus::Failed)
                .await
                .map(|_| ()),
        }
    }

    async fn start_iteration_with_deferred_goal(
        &self,
        workflow_id: &WorkflowId,
        iteration_goal: &str,
    ) -> Result<()> {
        let (next, coordinator) = self
            .create_iteration_with_coordinator(
                workflow_id,
                IterationCreationReason::DeferredGoalContinuation,
                iteration_goal,
            )
            .await?;
        if let Err(err) = coordinator.create_and_start_first_attempt().await {
            tracing::warn!(
                error = %err,
                workflow_id = %workflow_id.as_str(),
                iteration_id = %next.id.as_str(),
                "continuation first-attempt start failed; compensating workflow to failed",
            );
            self.iteration_coordinators.deregister(&next.id);
            self.deps
                .iteration_store
                .set_status(
                    &next.id,
                    IterationStatus::Cancelled,
                    Some(eos_types::UtcDateTime::now()),
                )
                .await?;
            self.close_workflow(workflow_id, WorkflowStatus::Failed)
                .await?;
        }
        Ok(())
    }

    async fn close_workflow(
        &self,
        workflow_id: &WorkflowId,
        status: WorkflowStatus,
    ) -> Result<Workflow> {
        let workflow = self.require_workflow(workflow_id).await?;
        if !workflow.is_open() {
            return Ok(workflow);
        }
        Ok(self
            .deps
            .workflow_store
            .set_status(workflow_id, status, Some(eos_types::UtcDateTime::now()))
            .await?)
    }

    async fn compensate_failed_start(
        &self,
        workflow_id: &WorkflowId,
        iteration_id: &eos_types::IterationId,
    ) -> Result<()> {
        if let Some(iteration) = self.deps.iteration_store.get(iteration_id).await? {
            if let Some(attempt_id) = iteration.latest_attempt_id() {
                if let Some(attempt) = self.deps.attempt_store.get(attempt_id).await? {
                    if !attempt.is_closed() {
                        self.deps
                            .attempt_store
                            .close(
                                attempt_id,
                                eos_types::AttemptClosure::Failed {
                                    reason: eos_types::AttemptFailReason::StartupFailed,
                                    closed_at: eos_types::UtcDateTime::now(),
                                },
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
                IterationStatus::Cancelled,
                Some(eos_types::UtcDateTime::now()),
            )
            .await?;
        self.deps
            .workflow_store
            .set_status(
                workflow_id,
                WorkflowStatus::Cancelled,
                Some(eos_types::UtcDateTime::now()),
            )
            .await?;
        self.iteration_coordinators.deregister(iteration_id);
        Ok(())
    }

    async fn require_workflow(&self, workflow_id: &WorkflowId) -> Result<Workflow> {
        self.deps
            .workflow_store
            .get(workflow_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("workflow", workflow_id.as_str()))
    }
}

#[async_trait]
impl WorkflowApi for WorkflowRun {
    async fn start_workflow(
        &self,
        request: StartWorkflowRequest,
    ) -> std::result::Result<StartedWorkflow, WorkflowApiError> {
        let started = self
            .start(
                &request.workflow_goal,
                &request.parent_task_id,
                &request.agent_run_id,
                request.tool_use_id.as_ref(),
            )
            .await
            .map_err(workflow_api_error)?;
        Ok(StartedWorkflow {
            workflow_id: started.workflow_id,
            workflow_goal: started.workflow_goal,
        })
    }

    async fn check_workflow_status(
        &self,
        workflow_id: &WorkflowId,
    ) -> std::result::Result<String, WorkflowApiError> {
        let workflow = self
            .require_workflow(workflow_id)
            .await
            .map_err(workflow_api_error)?;
        Ok(format!("{:?}", workflow.status).to_lowercase())
    }

    async fn cancel_workflow(
        &self,
        workflow_id: &WorkflowId,
        _reason: &str,
    ) -> std::result::Result<String, WorkflowApiError> {
        let workflow = self
            .close_workflow(workflow_id, WorkflowStatus::Cancelled)
            .await
            .map_err(workflow_api_error)?;
        Ok(format!("workflow {} cancelled", workflow.id.as_str()))
    }

    async fn poll_terminal_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> std::result::Result<Option<TerminalWorkflow>, WorkflowApiError> {
        let workflow = self
            .require_workflow(workflow_id)
            .await
            .map_err(workflow_api_error)?;
        let status = match workflow.status {
            WorkflowStatus::Open => return Ok(None),
            WorkflowStatus::Succeeded => WorkflowTerminalStatus::Completed,
            WorkflowStatus::Failed => WorkflowTerminalStatus::Failed,
            WorkflowStatus::Cancelled => WorkflowTerminalStatus::Cancelled,
        };
        Ok(Some(TerminalWorkflow {
            workflow_id: workflow.id,
            status,
        }))
    }

    async fn list_open_delegated_workflows_for_agent_run(
        &self,
        agent_run_id: &AgentRunId,
    ) -> std::result::Result<Vec<OpenDelegatedWorkflow>, WorkflowApiError> {
        let workflows = self
            .deps
            .workflow_store
            .list_for_launching_agent_run(agent_run_id)
            .await?;
        Ok(workflows
            .into_iter()
            .filter(eos_types::Workflow::is_open)
            .map(|workflow| OpenDelegatedWorkflow {
                workflow_id: workflow.id,
                workflow_goal: workflow.workflow_goal,
            })
            .collect())
    }

    async fn workflow_depth(
        &self,
        _workflow_id: &WorkflowId,
    ) -> std::result::Result<u32, WorkflowApiError> {
        Ok(1)
    }
}

fn workflow_api_error(err: WorkflowError) -> WorkflowApiError {
    match err {
        WorkflowError::Store(err) => WorkflowApiError::Store(err),
        other => WorkflowApiError::Internal(other.to_string()),
    }
}
