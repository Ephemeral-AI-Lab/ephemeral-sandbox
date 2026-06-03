use eos_state::{AttemptId, TaskId, TaskStatus, WorkflowStatus};

use crate::attempt::AttemptDeps;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use eos_state::{
        AttemptFailReason, AttemptStage, AttemptStatus, IterationStatus, TaskStatus, WorkflowStatus,
    };

    use super::*;
    use crate::testsupport::{
        agent_registry_without_planner, root_task, MemoryStores, QueueRunner,
    };

    // AC-eos-workflow-01: starting from a running parent creates
    // workflow + iteration (seq 1, goal = prompt) + first attempt and leaves
    // the parent task byte-identical (GC-eos-workflow-02).
    #[tokio::test]
    async fn start_leaves_parent_running() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let deps = stores.deps(runner);
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let before = stores.task(&parent.id).unwrap();

        let started = WorkflowStarter::new(deps)
            .start(" delegated goal ", &parent.id)
            .await
            .unwrap();

        // Parent row is untouched.
        assert_eq!(stores.task(&parent.id).unwrap(), before);

        let workflow = stores.workflow(&started.workflow_id).unwrap();
        assert_eq!(workflow.parent_task_id, parent.id);
        assert_eq!(workflow.workflow_goal, "delegated goal");
        let iteration = stores.iteration(&started.iteration_id).unwrap();
        assert!(iteration.is_open());
        assert_eq!(iteration.sequence_no, 1);
        assert_eq!(iteration.iteration_goal, "delegated goal");
        let attempt = stores.attempt(&started.attempt_id).unwrap();
        assert_eq!(attempt.stage, AttemptStage::Plan);
        assert_eq!(attempt.status, AttemptStatus::Running);
        assert!(stores
            .task(&crate::planner_task_id(&attempt.id).unwrap())
            .is_some());
    }

    // AC-eos-workflow-02: start rejects a blank prompt, a missing/non-running
    // parent, and a parent with an open delegated child, each as a distinct
    // error. (A "missing request id" is unrepresentable: `Task.request_id` is a
    // non-optional `RequestId`, eliminating the Python defensive branch.)
    #[tokio::test]
    async fn start_rejects_invalid_parent() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let deps = stores.deps(runner);
        let starter = WorkflowStarter::new(deps);

        // Blank prompt.
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        assert!(matches!(
            starter.start("   ", &parent.id).await.unwrap_err(),
            WorkflowError::BlankPrompt
        ));

        // Missing parent task.
        let err = starter
            .start("goal", &"ghost".parse().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkflowError::NotFound { entity: "task", .. }),
            "{err:?}"
        );

        // Non-running parent.
        let done = root_task("done", TaskStatus::Done);
        stores.seed_task(done.clone());
        let err = starter.start("goal", &done.id).await.unwrap_err();
        assert!(
            matches!(err, WorkflowError::Invariant(ref m) if m.contains("is not running")),
            "{err:?}"
        );

        // Parent that already has an open delegated child workflow.
        starter.start("first goal", &parent.id).await.unwrap();
        let err = starter.start("second goal", &parent.id).await.unwrap_err();
        assert!(
            matches!(err, WorkflowError::Invariant(ref m) if m.contains("open delegated workflow")),
            "{err:?}"
        );
    }

    // AC-eos-workflow-03: a failure during first-attempt creation runs the
    // compensation saga (attempt FAILED STARTUP_FAILED, iteration + workflow
    // CANCELLED, coordinator deregistered) and never mutates the parent.
    #[tokio::test]
    async fn compensation_rolls_back() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let mut deps = stores.deps(runner);
        // No `planner` profile -> the planner launch fails inside `start`.
        deps.agent_registry = Arc::new(agent_registry_without_planner());
        let coordinators = deps.iteration_coordinators.clone().unwrap();
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());

        let err = WorkflowStarter::new(deps)
            .start("delegated goal", &parent.id)
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkflowError::AgentDefinition(_)),
            "expected agent-definition launch failure, got {err:?}"
        );

        let workflow = eos_state::WorkflowStore::list_for_parent_task(stores.as_ref(), &parent.id)
            .await
            .unwrap()
            .pop()
            .expect("compensation leaves the workflow row");
        assert_eq!(workflow.status, WorkflowStatus::Cancelled);
        let iteration_id = workflow.iteration_ids.first().unwrap();
        let iteration = stores.iteration(iteration_id).unwrap();
        assert_eq!(iteration.status, IterationStatus::Cancelled);
        assert!(
            coordinators.get(iteration_id).is_none(),
            "coordinator deregistered"
        );
        let attempt_id = iteration.attempt_ids.first().unwrap();
        let attempt = stores.attempt(attempt_id).unwrap();
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(attempt.fail_reason, Some(AttemptFailReason::StartupFailed));
        // Parent untouched.
        assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
    }
}
