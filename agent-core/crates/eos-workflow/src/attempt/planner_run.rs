use std::sync::Arc;

use eos_types::{
    AttemptFailReason, AttemptStage, PlanOutcomeSubmission, Task, TaskId, TaskOutcome, TaskRole,
    TaskStatus,
};

use crate::{Result, WorkflowError};

use super::work_items::{execution_nodes, validate_work_items};
use super::work_items_run::WorkItemsRun;
use super::{AgentLaunch, AgentLaunchFactory, AgentRunReport, AttemptRun};

/// Planner launch and terminal-plan settlement for one attempt.
pub(crate) struct PlannerRun {
    attempt_run: Arc<AttemptRun>,
}

impl PlannerRun {
    pub(crate) fn new(attempt_run: Arc<AttemptRun>) -> Self {
        Self { attempt_run }
    }

    pub(crate) async fn start(&self) -> Result<()> {
        self.attempt_run.validate_run_concurrency()?;
        let attempt = self.attempt_run.assert_stage(AttemptStage::Plan).await?;
        if attempt.planner_task_id().is_some() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} already has a planner task",
                attempt.id.as_str()
            )));
        }

        let task_id = TaskId::new_v4();
        let launch = AgentLaunchFactory::new(self.attempt_run.deps().clone())
            .for_planner(&attempt, task_id.clone())
            .await?;
        self.attempt_run
            .deps()
            .active_attempt_runs
            .register(Arc::clone(&self.attempt_run))?;

        let result: Result<()> = async {
            self.attempt_run
                .deps()
                .task_store
                .insert_task(&Task {
                    id: task_id.clone(),
                    request_id: launch.request_id.clone(),
                    role: TaskRole::Planner,
                    instruction: launch.instruction.clone(),
                    status: TaskStatus::Running,
                    agent_name: Some(launch.agent_name.as_str().to_owned()),
                    task_outcome: None,
                })
                .await?;
            self.attempt_run
                .deps()
                .attempt_store
                .bind_planner_task(&attempt.id, &task_id)
                .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.attempt_run
                .deps()
                .active_attempt_runs
                .deregister(&attempt.id);
        }
        result?;
        self.spawn_planner_run(launch);
        Ok(())
    }

    pub(crate) async fn record_plan_outcome(
        &self,
        submission: PlanOutcomeSubmission,
    ) -> Result<()> {
        self.attempt_run
            .assert_submission_attempt(&submission.attempt_id)?;
        self.attempt_run.validate_run_concurrency()?;
        let attempt = self.attempt_run.assert_stage(AttemptStage::Plan).await?;
        let planner_task_id = attempt.planner_task_id().cloned().ok_or_else(|| {
            WorkflowError::invariant(format!(
                "attempt {:?} has no planner task",
                attempt.id.as_str()
            ))
        })?;
        validate_work_items(
            &submission.work_items,
            &self.attempt_run.deps().agent_registry,
        )?;
        let planner_task = self
            .attempt_run
            .deps()
            .task_store
            .get(&planner_task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("planner task", planner_task_id.as_str()))?;
        if planner_task.role != TaskRole::Planner || planner_task.status != TaskStatus::Running {
            return Err(WorkflowError::invariant(format!(
                "planner task {:?} is not running",
                planner_task_id.as_str()
            )));
        }

        let task_outcome = TaskOutcome::Planner {
            plan_spec: submission.plan_spec.clone(),
            work_items: submission.work_items.clone(),
            deferred_goal_for_next_iteration: submission.deferred_goal_for_next_iteration.clone(),
        };
        self.attempt_run
            .deps()
            .task_store
            .set_task_status_if_current(
                &planner_task_id,
                TaskStatus::Running,
                TaskStatus::Done,
                Some(&task_outcome),
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "planner task {:?} is no longer running",
                    planner_task_id.as_str()
                ))
            })?;
        let nodes = execution_nodes(&submission.work_items);
        self.attempt_run
            .deps()
            .attempt_store
            .record_plan_nodes(&attempt.id, &nodes)
            .await?;
        WorkItemsRun::new(Arc::clone(&self.attempt_run))
            .advance()
            .await
    }

    fn spawn_planner_run(&self, launch: AgentLaunch) {
        let attempt_run = Arc::clone(&self.attempt_run);
        let runner = self.attempt_run.deps().runner.clone();
        tokio::spawn(async move {
            let report = runner.run(launch.clone()).await;
            if let Err(err) = PlannerRun::new(attempt_run)
                .settle_planner(launch, report)
                .await
            {
                tracing::warn!(error = %err, "planner run could not be settled");
            }
        });
    }

    async fn settle_planner(
        &self,
        launch: AgentLaunch,
        report: Result<AgentRunReport>,
    ) -> Result<()> {
        if let Err(err) = &report {
            tracing::warn!(
                attempt_id = %self.attempt_run.attempt_id().as_str(),
                task_id = %launch.task_id.as_str(),
                error = %err,
                "planner run failed"
            );
        }
        if let Ok(report) = &report {
            if let Some(summary) = &report.failure_summary {
                tracing::warn!(
                    attempt_id = %self.attempt_run.attempt_id().as_str(),
                    task_id = %launch.task_id.as_str(),
                    %summary,
                    "planner run reported a failure summary"
                );
            }
        }

        let Some(task) = self
            .attempt_run
            .deps()
            .task_store
            .get(&launch.task_id)
            .await?
        else {
            return Err(WorkflowError::not_found(
                "planner task",
                launch.task_id.as_str(),
            ));
        };
        match task.status {
            TaskStatus::Done => {
                WorkItemsRun::new(Arc::clone(&self.attempt_run))
                    .advance()
                    .await
            }
            TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Cancelled => {
                self.attempt_run
                    .close_attempt_failed(AttemptFailReason::TaskFailed)
                    .await
            }
            TaskStatus::Pending | TaskStatus::Running => {
                self.synthesize_planner_failure(&launch).await
            }
        }
    }

    async fn synthesize_planner_failure(&self, launch: &AgentLaunch) -> Result<()> {
        self.attempt_run
            .deps()
            .task_store
            .set_task_status_if_current(
                &launch.task_id,
                TaskStatus::Running,
                TaskStatus::Failed,
                None,
            )
            .await?;
        self.attempt_run
            .close_attempt_failed(AttemptFailReason::TaskFailed)
            .await
    }
}
