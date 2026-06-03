use std::sync::Arc;

use eos_agent_def::AgentRole;
use eos_state::{
    execution_outcome_for_submission, Attempt, ExecutionRole, GeneratorSubmission, JsonObject,
    PlannerFailReason, PlannerFailureSubmission, ReducerSubmission, Task, TaskOutcomeStatus,
    TaskRole, TaskStatus,
};
use serde_json::Value;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::attempt::plan_dag::{dag_status, ready_pending_plan_ids};
use crate::attempt::{AgentLaunch, AgentLaunchFactory, AgentRunReport, AgentTerminal, AttemptDeps};
use crate::{Result, WorkflowError};

use super::AttemptOrchestrator;

/// Single-writer RUN-stage scheduler for one Attempt.
#[derive(Debug, Clone)]
pub struct AttemptStageAdvancer {
    orchestrator: Arc<AttemptOrchestrator>,
    cancel: CancellationToken,
}

impl AttemptStageAdvancer {
    /// Create a scheduler for an orchestrator.
    #[must_use]
    pub fn new(orchestrator: Arc<AttemptOrchestrator>) -> Self {
        Self {
            orchestrator,
            cancel: CancellationToken::new(),
        }
    }

    /// Drive RUN-stage tasks to quiescence or until no locally-spawned running
    /// task is available to join.
    ///
    /// # Errors
    /// Returns [`WorkflowError`] for persisted DAG/store invariants.
    pub async fn advance_run_stage(&self) -> Result<()> {
        let deps = self.orchestrator.deps().clone();
        if deps.max_concurrent_task_runs == 0 {
            return Err(WorkflowError::invariant(
                "max_concurrent_task_runs must be at least 1",
            ));
        }
        let mut set = JoinSet::new();
        loop {
            let attempt = self.orchestrator.fresh_attempt().await?;
            if attempt.is_closed() || attempt.stage != eos_state::AttemptStage::Run {
                return Ok(());
            }
            let tasks = self.orchestrator.plan_task_records(&attempt).await?;
            for task_id in ready_pending_plan_ids(&tasks)? {
                if set.len() >= deps.max_concurrent_task_runs {
                    break;
                }
                let task = deps
                    .task_store
                    .set_task_status(&task_id, TaskStatus::Running, None, None)
                    .await?;
                let launch = match self.build_launch(&deps, &attempt, &task).await {
                    Ok(launch) => launch,
                    Err(err) => {
                        self.mark_launch_failed(&task, &err.to_string()).await?;
                        continue;
                    }
                };
                let runner = deps.runner.clone();
                set.spawn(async move {
                    let result = runner.run(launch.clone()).await;
                    (launch, result)
                });
            }

            let refreshed = self.orchestrator.fresh_attempt().await?;
            let refreshed_tasks = self.orchestrator.plan_task_records(&refreshed).await?;
            let status = dag_status(&refreshed_tasks)?;
            if status.all_quiescent {
                if status.any_failed_or_blocked {
                    return self
                        .orchestrator
                        .close_attempt(
                            eos_state::AttemptStatus::Failed,
                            Some(eos_state::AttemptFailReason::TaskFailed),
                        )
                        .await;
                }
                if status.all_done {
                    return self
                        .orchestrator
                        .close_attempt(eos_state::AttemptStatus::Passed, None)
                        .await;
                }
            }
            if set.is_empty() {
                return Ok(());
            }
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    set.abort_all();
                    return Ok(());
                }
                Some(joined) = set.join_next() => {
                    let (launch, report) = joined
                        .map_err(|err| WorkflowError::Join(err.to_string()))?;
                    self.apply_report(launch, report).await?;
                }
            }
        }
    }

    async fn build_launch(
        &self,
        deps: &AttemptDeps,
        attempt: &Attempt,
        task: &Task,
    ) -> Result<AgentLaunch> {
        if task.role == TaskRole::Reducer {
            AgentLaunchFactory::new(deps.clone())
                .for_reducer(attempt, task)
                .await
        } else {
            let agent_name = task.agent_name.clone().ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "task {:?} has no persisted agent profile",
                    task.id.as_str()
                ))
            })?;
            AgentLaunchFactory::new(deps.clone())
                .for_generator(attempt, task, &agent_name)
                .await
        }
    }

    async fn mark_launch_failed(&self, task: &Task, summary: &str) -> Result<()> {
        if task.attempt_id.is_none() {
            return Err(WorkflowError::invariant(format!(
                "task {:?} launch failure requires task.attempt_id",
                task.id.as_str()
            )));
        }
        let outcome = format!("agent launch failed: {summary}");
        let terminal_tool_result = json_object("fail_reason", "agent_launch_failed");
        match task.role {
            TaskRole::Generator => {
                let result = execution_outcome_for_submission(
                    task.id.clone(),
                    ExecutionRole::Generator,
                    TaskOutcomeStatus::Failed,
                    outcome.clone(),
                );
                let outcomes = [result];
                self.orchestrator
                    .deps()
                    .task_store
                    .set_task_status(
                        &task.id,
                        TaskStatus::Failed,
                        Some(&outcomes),
                        Some(&terminal_tool_result),
                    )
                    .await?;
                Ok(())
            }
            TaskRole::Reducer => {
                let result = execution_outcome_for_submission(
                    task.id.clone(),
                    ExecutionRole::Reducer,
                    TaskOutcomeStatus::Failed,
                    outcome,
                );
                let outcomes = [result];
                self.orchestrator
                    .deps()
                    .task_store
                    .set_task_status(
                        &task.id,
                        TaskStatus::Failed,
                        Some(&outcomes),
                        Some(&terminal_tool_result),
                    )
                    .await?;
                Ok(())
            }
            _ => Err(WorkflowError::invariant(format!(
                "task {:?} has unsupported launch-failure role {:?}",
                task.id.as_str(),
                task.role
            ))),
        }
    }

    async fn apply_report(
        &self,
        launch: AgentLaunch,
        report: Result<AgentRunReport>,
    ) -> Result<()> {
        match report {
            Ok(report) => {
                if let Some(terminal) = report.terminal {
                    self.apply_terminal(terminal).await
                } else {
                    self.synthesize_failure(
                        &launch,
                        report
                            .failure_summary
                            .as_deref()
                            .unwrap_or("agent run ended without a terminal submission"),
                    )
                    .await
                }
            }
            Err(err) => {
                self.synthesize_failure(&launch, &format!("agent run failed: {err}"))
                    .await
            }
        }
    }

    async fn apply_terminal(&self, terminal: AgentTerminal) -> Result<()> {
        match terminal {
            AgentTerminal::Planner(_) => Err(WorkflowError::invariant(
                "planner plan submission cannot be applied during run stage",
            )),
            AgentTerminal::PlannerFailure(submission) => {
                self.orchestrator.apply_planner_failure(submission).await
            }
            AgentTerminal::Generator(submission) => {
                self.orchestrator
                    .record_generator_submission(submission)
                    .await
            }
            AgentTerminal::Reducer(submission) => {
                self.orchestrator
                    .record_reducer_submission(submission)
                    .await
            }
        }
    }

    async fn synthesize_failure(&self, launch: &AgentLaunch, summary: &str) -> Result<()> {
        let attempt_id = launch.attempt_id.clone().ok_or_else(|| {
            WorkflowError::invariant(format!(
                "role {:?} exhaustion report requires launch.attempt_id",
                launch.role
            ))
        })?;
        let exhausted = json_object("fail_reason", "run_exhausted");
        match launch.role {
            AgentRole::Planner => {
                self.orchestrator
                    .apply_planner_failure(PlannerFailureSubmission {
                        attempt_id,
                        planner_task_id: launch.task_id.clone(),
                        fail_reason: PlannerFailReason::RunExhausted,
                    })
                    .await
            }
            AgentRole::Generator => {
                self.orchestrator
                    .record_generator_submission(GeneratorSubmission {
                        attempt_id,
                        task_id: launch.task_id.clone(),
                        status: TaskOutcomeStatus::Failed,
                        outcome: summary.to_owned(),
                        terminal_tool_result: exhausted,
                    })
                    .await
            }
            AgentRole::Reducer => {
                self.orchestrator
                    .record_reducer_submission(ReducerSubmission {
                        attempt_id,
                        task_id: launch.task_id.clone(),
                        status: TaskOutcomeStatus::Failed,
                        outcome: summary.to_owned(),
                        terminal_tool_result: exhausted,
                    })
                    .await
            }
            _ => Err(WorkflowError::invariant(format!(
                "no exhaustion reporter for role {:?}",
                launch.role
            ))),
        }
    }
}

fn json_object(key: &str, value: impl Into<Value>) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert(key.to_owned(), value.into());
    object
}
