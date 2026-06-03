use std::sync::Arc;

use eos_agent_def::AgentRole;
use eos_state::{
    execution_outcome_for_submission, Attempt, ExecutionRole, GeneratorSubmission,
    PlannerFailReason, PlannerFailureSubmission, ReducerSubmission, Task, TaskOutcomeStatus,
    TaskRole, TaskStatus,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::attempt::plan_dag::{dag_status, ready_pending_plan_ids};
use crate::attempt::{AgentLaunch, AgentLaunchFactory, AgentRunReport, AttemptDeps};
use crate::util::json_object;
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
                    self.settle_run_task(launch, report).await?;
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

    /// Settle a RUN-stage task after its run resolves (Path A-recording). The
    /// submit tool already recorded the agent's outcome (task Done/Failed) via
    /// the recording port *during* the run, so the loop's only post-join job is
    /// Python's still-RUNNING exhaustion guard: a task still `Running` means the
    /// agent died without submitting -> synthesize `run_exhausted`. A recorded
    /// task is a no-op (the tool already wrote it).
    async fn settle_run_task(
        &self,
        launch: AgentLaunch,
        report: Result<AgentRunReport>,
    ) -> Result<()> {
        let task = self
            .orchestrator
            .deps()
            .task_store
            .get(&launch.task_id)
            .await?;
        if matches!(task, Some(ref task) if task.status == TaskStatus::Running) {
            let summary = match report {
                Ok(report) => report
                    .failure_summary
                    .unwrap_or_else(|| "agent run ended without a terminal submission".to_owned()),
                Err(err) => format!("agent run failed: {err}"),
            };
            self.synthesize_failure(&launch, &summary).await
        } else {
            Ok(())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use eos_agent_def::AgentRole;
    use eos_state::{
        AttemptFailReason, AttemptStage, AttemptStatus, IterationStatus, Task, TaskOutcomeStatus,
        TaskRole, TaskStatus, WorkflowStatus,
    };
    use serde_json::json;

    use super::AttemptStageAdvancer;
    use crate::ids::generator_task_id;
    use crate::testsupport::{
        one_step_plan, root_task, wait_for_workflow_status, MemoryStores, QueueRunner,
        ScriptedRunner,
    };
    use crate::{AgentRunReport, AgentTerminal, WorkflowStarter};

    // AC-eos-workflow-08: the run is exercised entirely through the injected
    // `AgentRunner` double (no eos-engine edge); the seam hands each role a
    // well-formed launch.
    #[tokio::test]
    async fn injected_runner_double() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let mut deps = stores.deps(runner.clone());
        deps.lifecycle_config.default_attempt_budget = 1;
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let started = WorkflowStarter::new(deps)
            .start("delegated goal", &parent.id)
            .await
            .unwrap();
        let generator_id = generator_task_id(&started.attempt_id, "g1").unwrap();
        runner.push(AgentRunReport::terminal(AgentTerminal::Planner(
            one_step_plan(&started),
        )));
        runner.push(AgentRunReport::terminal(AgentTerminal::Generator(
            eos_state::GeneratorSubmission {
                attempt_id: started.attempt_id.clone(),
                task_id: generator_id.clone(),
                status: TaskOutcomeStatus::Success,
                outcome: "generated".to_owned(),
                terminal_tool_result: crate::testsupport::terminal_result(),
            },
        )));
        runner.push(AgentRunReport::terminal(AgentTerminal::Reducer(
            eos_state::ReducerSubmission {
                attempt_id: started.attempt_id.clone(),
                task_id: crate::reducer_task_id(&started.attempt_id, "r1").unwrap(),
                status: TaskOutcomeStatus::Success,
                outcome: "reduced".to_owned(),
                terminal_tool_result: crate::testsupport::terminal_result(),
            },
        )));
        wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Succeeded).await;

        let launches = runner.launches();
        assert_eq!(launches.len(), 3);
        assert_eq!(launches[0].role, AgentRole::Planner);
        assert_eq!(launches[1].role, AgentRole::Generator);
        assert_eq!(launches[1].task_id, generator_id);
        assert_eq!(launches[1].attempt_id.as_ref(), Some(&started.attempt_id));
        assert_eq!(launches[2].role, AgentRole::Reducer);
    }

    // AC-eos-workflow-07 (liveness): a generator run that ends WITHOUT a terminal
    // submission is mapped to a synthesized failure; the attempt advances to a
    // terminal state instead of hanging.
    #[tokio::test]
    async fn dead_agent_synthesizes_failure() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let mut deps = stores.deps(runner.clone());
        deps.lifecycle_config.default_attempt_budget = 1;
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let started = WorkflowStarter::new(deps)
            .start("delegated goal", &parent.id)
            .await
            .unwrap();
        runner.push(AgentRunReport::terminal(AgentTerminal::Planner(
            one_step_plan(&started),
        )));
        runner.push(AgentRunReport::no_terminal(
            "generator ended without terminal",
        ));
        wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Failed).await;

        let attempt = stores.attempt(&started.attempt_id).unwrap();
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(attempt.fail_reason, Some(AttemptFailReason::TaskFailed));
        let generator_id = generator_task_id(&started.attempt_id, "g1").unwrap();
        let task = stores.task(&generator_id).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(
            task.terminal_tool_result.unwrap().get("fail_reason"),
            Some(&json!("run_exhausted"))
        );
        assert_eq!(
            stores.iteration(&started.iteration_id).unwrap().status,
            IterationStatus::Failed
        );
        assert_eq!(
            stores.workflow(&started.workflow_id).unwrap().status,
            WorkflowStatus::Failed
        );
    }

    // AC-eos-workflow-07 (liveness, planner): a planner run with no terminal is
    // synthesized into a planner failure (run_exhausted) and the attempt fails.
    #[tokio::test]
    async fn dead_planner_synthesizes_failure() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let mut deps = stores.deps(runner.clone());
        deps.lifecycle_config.default_attempt_budget = 1;
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let started = WorkflowStarter::new(deps)
            .start("delegated goal", &parent.id)
            .await
            .unwrap();
        runner.push(AgentRunReport::no_terminal(
            "planner ended without terminal",
        ));
        wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Failed).await;

        let attempt = stores.attempt(&started.attempt_id).unwrap();
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(attempt.fail_reason, Some(AttemptFailReason::TaskFailed));
        let planner_task = stores
            .task(&crate::planner_task_id(&started.attempt_id).unwrap())
            .unwrap();
        assert_eq!(planner_task.status, TaskStatus::Failed);
        assert_eq!(
            planner_task
                .terminal_tool_result
                .unwrap()
                .get("fail_reason"),
            Some(&json!("run_exhausted"))
        );
    }

    // AC-eos-workflow-08b (per-attempt fan-out cap): with 10 ready generators and
    // max_concurrent_task_runs = 3, no more than 3 agent runs are in flight at
    // once; surplus ready tasks stay pending, and the reducer runs only after all
    // its generator needs are done.
    #[tokio::test]
    async fn fanout_respects_concurrency_cap() {
        let stores = Arc::new(MemoryStores::default());
        let runner = ScriptedRunner::new(10, TaskOutcomeStatus::Success, 0, "");
        let mut deps = stores.deps(runner.clone());
        deps.lifecycle_config.default_attempt_budget = 1;
        deps.max_concurrent_task_runs = 3;
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let started = WorkflowStarter::new(deps)
            .start("delegated goal", &parent.id)
            .await
            .unwrap();
        wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Succeeded).await;

        // The cap binds exactly: with 10 ready generators the scheduler keeps 3
        // runs in flight (never more — the ceiling; never fewer once saturated —
        // proving launches are not serialized). Deterministic on the
        // current-thread test runtime: 3 are spawned before the first
        // `join_next().await`, and each runner future yields before completing,
        // so all 3 enter before any exits.
        assert_eq!(
            runner.max_in_flight(),
            3,
            "expected the per-attempt cap of 3 to be saturated, got {}",
            runner.max_in_flight()
        );
        assert_eq!(
            stores.attempt(&started.attempt_id).unwrap().status,
            AttemptStatus::Passed
        );

        let launches = runner.launches();
        // planner + 10 generators + 1 reducer.
        assert_eq!(launches.len(), 12);
        assert_eq!(launches[0].role, AgentRole::Planner);
        assert_eq!(launches.last().unwrap().role, AgentRole::Reducer);
        let generators = launches[1..11]
            .iter()
            .filter(|l| l.role == AgentRole::Generator)
            .count();
        assert_eq!(generators, 10, "all generators ran before the reducer");
    }

    // The launcher marks a task FAILED (instead of stranding it RUNNING) when its
    // launch context cannot be built (here: a generator with no agent profile).
    #[tokio::test]
    async fn launch_failure_marks_task_failed() {
        let stores = Arc::new(MemoryStores::default());
        let runner = Arc::new(QueueRunner::default());
        let mut deps = stores.deps(runner);
        deps.lifecycle_config.default_attempt_budget = 1;
        let parent = root_task("parent", TaskStatus::Running);
        stores.seed_task(parent.clone());
        let started = WorkflowStarter::new(deps.clone())
            .start("delegated goal", &parent.id)
            .await
            .unwrap();
        let task_id = generator_task_id(&started.attempt_id, "missing-profile").unwrap();
        stores.seed_task(Task {
            id: task_id.clone(),
            request_id: parent.request_id,
            role: TaskRole::Generator,
            instruction: "do work".to_owned(),
            status: TaskStatus::Pending,
            workflow_id: Some(started.workflow_id.clone()),
            iteration_id: Some(started.iteration_id.clone()),
            attempt_id: Some(started.attempt_id.clone()),
            agent_name: None,
            needs: Vec::new(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        });
        eos_state::AttemptStore::set_generator_task_ids(
            stores.as_ref(),
            &started.attempt_id,
            std::slice::from_ref(&task_id),
        )
        .await
        .unwrap();
        eos_state::AttemptStore::set_stage(stores.as_ref(), &started.attempt_id, AttemptStage::Run)
            .await
            .unwrap();

        let orchestrator = deps.orchestrator_registry.get(&started.attempt_id).unwrap();
        AttemptStageAdvancer::new(orchestrator)
            .advance_run_stage()
            .await
            .unwrap();

        let task = stores.task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(
            task.terminal_tool_result.unwrap().get("fail_reason"),
            Some(&json!("agent_launch_failed"))
        );
        assert_eq!(
            stores.attempt(&started.attempt_id).unwrap().status,
            AttemptStatus::Failed
        );
        assert_eq!(
            stores.workflow(&started.workflow_id).unwrap().status,
            WorkflowStatus::Failed
        );
    }
}
