//! Delegated-workflow lifecycle service implementing [`eos_types::WorkflowApi`].
//!
//! Workflows are addressed by their natural [`WorkflowId`]; there is no synthetic
//! `wf_<n>` session handle. Internal helpers raise the rich [`WorkflowError`];
//! the public API methods map it onto [`WorkflowApiError`] at the trait boundary.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{
    AgentRunId, AttemptClosure, CancelPort, IterationStatus, OutstandingWorkflow,
    StartWorkflowRequest, StartedWorkflow, TaskId, TaskStore, TerminalWorkflow, WorkflowApi,
    WorkflowApiError, WorkflowId, WorkflowStatus, WorkflowTerminalStatus,
};

use crate::{WorkflowError, WorkflowStarter};

/// Workflow lifecycle service over delegated workflow state.
#[derive(Clone)]
pub struct WorkflowService {
    starter: WorkflowStarter,
    workflow_store: Arc<dyn eos_types::WorkflowStore>,
    iteration_store: Arc<dyn eos_types::IterationStore>,
    attempt_store: Arc<dyn eos_types::AttemptStore>,
    task_store: Arc<dyn TaskStore>,
    /// The recursive cancellation port (spec §12.4): workflow cancellation
    /// decomposes through `cancel_task` rather than flipping task rows directly.
    cancel_port: Arc<dyn CancelPort>,
}

impl std::fmt::Debug for WorkflowService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowService").finish_non_exhaustive()
    }
}

impl WorkflowService {
    /// Create a workflow service over the delegated workflow stores.
    #[must_use]
    pub fn new(
        starter: WorkflowStarter,
        workflow_store: Arc<dyn eos_types::WorkflowStore>,
        iteration_store: Arc<dyn eos_types::IterationStore>,
        attempt_store: Arc<dyn eos_types::AttemptStore>,
        task_store: Arc<dyn TaskStore>,
        cancel_port: Arc<dyn CancelPort>,
    ) -> Self {
        Self {
            starter,
            workflow_store,
            iteration_store,
            attempt_store,
            task_store,
            cancel_port,
        }
    }
}

#[async_trait]
impl WorkflowApi for WorkflowService {
    async fn start_workflow(
        &self,
        request: StartWorkflowRequest,
    ) -> Result<StartedWorkflow, WorkflowApiError> {
        let started = self
            .starter
            .start(&request.workflow_goal, &request.parent_task_id)
            .await
            .map_err(workflow_api_error)?;
        Ok(StartedWorkflow {
            workflow_id: started.workflow_id,
            workflow_goal: request.workflow_goal,
        })
    }

    async fn check_workflow_status(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<String, WorkflowApiError> {
        self.workflow_status_text(workflow_id)
            .await
            .map_err(workflow_api_error)
    }

    async fn cancel_workflow(
        &self,
        workflow_id: &WorkflowId,
        reason: &str,
    ) -> Result<String, WorkflowApiError> {
        self.cancel_workflow_inner(workflow_id, reason)
            .await
            .map_err(workflow_api_error)
    }

    async fn poll_terminal_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<TerminalWorkflow>, WorkflowApiError> {
        let Some(workflow) = self.workflow_store.get(workflow_id).await? else {
            return Ok(None);
        };
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

    async fn find_outstanding_workflows(
        &self,
        parent_task_id: &TaskId,
        _agent_run_id: &AgentRunId,
    ) -> Result<Vec<OutstandingWorkflow>, WorkflowApiError> {
        Ok(self
            .workflow_store
            .list_for_parent_task(parent_task_id)
            .await?
            .into_iter()
            .filter(eos_types::Workflow::is_open)
            .map(|workflow| OutstandingWorkflow {
                workflow_id: workflow.id,
                workflow_goal: workflow.workflow_goal,
            })
            .collect())
    }

    async fn workflow_depth(&self, workflow_id: &WorkflowId) -> Result<u32, WorkflowApiError> {
        // Walk delegation ancestry via each workflow's parent task's owning
        // workflow (`task.workflow_id`), counting hops; 1 = top-level. The `seen`
        // guard stops a malformed cycle from looping forever (Rust parity).
        let mut depth: u32 = 1;
        let mut current = workflow_id.clone();
        let mut seen = std::collections::HashSet::new();
        while seen.insert(current.clone()) {
            let Some(workflow) = self.workflow_store.get(&current).await? else {
                break;
            };
            let Some(parent) = self.task_store.get(&workflow.parent_task_id).await? else {
                break;
            };
            match parent.workflow_id {
                Some(parent_workflow_id) => {
                    depth += 1;
                    current = parent_workflow_id;
                }
                None => break,
            }
        }
        Ok(depth)
    }
}

impl WorkflowService {
    async fn workflow_status_text(&self, workflow_id: &WorkflowId) -> crate::Result<String> {
        let Some(workflow) = self.workflow_store.get(workflow_id).await? else {
            return Ok(format!("Workflow {workflow_id} was not found."));
        };
        let mut text = format!(
            "Workflow {} is {:?}. Goal: {}",
            workflow.id, workflow.status, workflow.workflow_goal
        );
        if let Some(outcomes) = &workflow.outcomes {
            text.push_str("\nOutcomes:\n");
            text.push_str(outcomes);
        }
        Ok(text)
    }

    async fn cancel_workflow_inner(
        &self,
        workflow_id: &WorkflowId,
        reason: &str,
    ) -> crate::Result<String> {
        let Some(workflow) = self.workflow_store.get(workflow_id).await? else {
            return Ok(format!("Workflow {workflow_id} was not found."));
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

    /// Decompose workflow cancellation through `cancel_iteration` -> `cancel_attempt`
    /// -> `cancel_task` (spec §12.4). Walks only *open* iterations / *non-closed*
    /// attempts (the idempotency guards), so a re-entrant cancel (a child workflow
    /// cancelled while tearing down a parent) terminates.
    async fn cancel_workflow_state(
        &self,
        workflow: &eos_types::Workflow,
        reason: &str,
    ) -> crate::Result<()> {
        let now = eos_types::UtcDateTime::now();
        // Iteration/workflow `outcomes` columns are read back strictly as
        // `Vec<ExecutionTaskOutcome>` by `ContextEngine`, so the cancellation
        // summary is the empty typed projection; the reason rides on each
        // cancelled task row and the `cancel` return string.
        const EMPTY_OUTCOMES: &str = "[]";

        for iteration in self.iteration_store.list_for_workflow(&workflow.id).await? {
            if !iteration.is_open() {
                continue;
            }
            for attempt in self.attempt_store.list_for_iteration(&iteration.id).await? {
                if attempt.is_closed() {
                    continue;
                }
                self.cancel_attempt(&attempt, reason, now).await?;
            }
            self.iteration_store
                .set_status(
                    &iteration.id,
                    IterationStatus::Cancelled,
                    Some(now),
                    Some(EMPTY_OUTCOMES),
                )
                .await?;
        }
        self.workflow_store
            .set_status(
                &workflow.id,
                WorkflowStatus::Cancelled,
                Some(now),
                Some(EMPTY_OUTCOMES),
            )
            .await?;
        Ok(())
    }

    /// Cancel one attempt (spec §12.4): latch every planner/generator/reducer task
    /// row to `Cancelled` *before* any teardown (closing the scheduler gap), then
    /// recurse `cancel_task` per task to tear down any live agent run, then close
    /// the attempt as `Cancelled`.
    async fn cancel_attempt(
        &self,
        attempt: &eos_types::Attempt,
        reason: &str,
        now: eos_types::UtcDateTime,
    ) -> crate::Result<()> {
        // Stop the planner orchestrator from materializing NEW (un-latched) task
        // rows. This is *not* redundant with `cancel_task(planner)`: the latch only
        // covers rows that exist at latch time, so the planner must be prevented
        // from creating fresh launchable rows after the latch.
        self.starter
            .orchestrator_registry()
            .abort_planner(&attempt.id);
        let tasks: Vec<eos_types::TaskId> = attempt
            .planner_task_id()
            .into_iter()
            .chain(attempt.generator_task_ids().iter())
            .chain(attempt.reducer_task_ids().iter())
            .cloned()
            .collect();
        // Latch BEFORE teardown so the scheduler sees terminal rows and cannot
        // launch a sibling into the cancellation window.
        self.task_store
            .latch_attempt_tasks_cancelled(&attempt.id, &tasks)
            .await?;
        // Tear down each task's live agent run. The status CAS inside `cancel_task`
        // no-ops (already latched `Cancelled`), but the live-run teardown still runs.
        for task_id in &tasks {
            self.cancel_port
                .cancel_task(task_id, reason)
                .await
                .map_err(|err| WorkflowError::Invariant(err.to_string()))?;
        }
        self.attempt_store
            .close(
                &attempt.id,
                AttemptClosure::Cancelled {
                    reason: reason.to_owned(),
                    outcomes: Vec::new(),
                    closed_at: now,
                },
            )
            .await?;
        Ok(())
    }
}

fn workflow_api_error(err: WorkflowError) -> WorkflowApiError {
    match err {
        WorkflowError::Store(err) => WorkflowApiError::Store(err),
        other => WorkflowApiError::Internal(other.to_string()),
    }
}

#[cfg(test)]
#[path = "../tests/service/mod.rs"]
mod tests;
