use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{GeneratorSubmission, ReducerSubmission, TaskStore, WorkflowId, WorkflowStatus};
use eos_tools::{
    OutstandingWorkflow, PlanSubmissionPort, PlannerPlan, SubmissionAck, ToolError,
    WorkflowControlPort,
};
use eos_types::WorkflowSessionId;

use crate::attempt::AttemptOrchestratorRegistry;
use crate::{WorkflowError, WorkflowStarter};

/// Adapter from `eos-tools` planner/generator/reducer terminal ports to active
/// per-attempt orchestrators.
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
        submission_ack(orchestrator.apply_plan(plan).await)
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
        submission_ack(orchestrator.apply_generator_submission(submission).await)
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
        submission_ack(orchestrator.apply_reducer_submission(submission).await)
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
    task_store: Arc<dyn TaskStore>,
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
        task_store: Arc<dyn TaskStore>,
    ) -> Self {
        Self {
            starter,
            workflow_store,
            task_store,
        }
    }
}

impl eos_tools::ports::Sealed for WorkflowControlAdapter {}

#[async_trait]
impl WorkflowControlPort for WorkflowControlAdapter {
    async fn start(
        &self,
        parent_task_id: &eos_state::TaskId,
        agent_id: &str,
        workflow_goal: &str,
    ) -> Result<eos_tools::StartedWorkflow, ToolError> {
        let _ = agent_id;
        let started = self
            .starter
            .start(workflow_goal, parent_task_id)
            .await
            .map_err(workflow_control_error)?;
        Ok(eos_tools::StartedWorkflow {
            workflow_task_id: workflow_handle(&started.workflow_id)?,
            workflow_id: started.workflow_id,
        })
    }

    async fn status(
        &self,
        workflow_id: &WorkflowId,
        workflow_task_id: Option<&WorkflowSessionId>,
    ) -> Result<String, ToolError> {
        if let Some(handle) = workflow_task_id {
            let Some(handle_workflow_id) = workflow_id_from_handle(handle) else {
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
        let handle = workflow_handle(&workflow.id)?;
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
        let Some(workflow_id) = workflow_id_from_handle(workflow_task_id) else {
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
        self.workflow_store
            .set_status(
                &workflow_id,
                WorkflowStatus::Cancelled,
                Some(eos_state::UtcDateTime::now()),
                None,
            )
            .await?;
        Ok(format!("Workflow {workflow_id} cancelled: {reason}"))
    }

    async fn find_outstanding(
        &self,
        parent_task_id: &eos_state::TaskId,
        agent_id: &str,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
        let _ = agent_id;
        self.workflow_store
            .list_for_parent_task(parent_task_id)
            .await?
            .into_iter()
            .filter(eos_state::Workflow::is_open)
            .map(|workflow| {
                Ok(OutstandingWorkflow {
                    workflow_task_id: workflow_handle(&workflow.id)?,
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

fn workflow_control_error(err: WorkflowError) -> ToolError {
    match err {
        WorkflowError::Store(err) => ToolError::Store(err),
        WorkflowError::Tool(err) => err,
        other => ToolError::Internal(other.to_string()),
    }
}

fn workflow_handle(workflow_id: &WorkflowId) -> Result<WorkflowSessionId, ToolError> {
    Ok(format!("wf_{}", workflow_id.as_str()).parse()?)
}

fn workflow_id_from_handle(handle: &WorkflowSessionId) -> Option<WorkflowId> {
    handle
        .as_str()
        .strip_prefix("wf_")
        .and_then(|id| id.parse().ok())
}
