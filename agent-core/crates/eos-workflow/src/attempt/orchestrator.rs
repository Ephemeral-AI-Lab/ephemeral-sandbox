use std::collections::BTreeMap;
use std::sync::Arc;

use eos_types::{
    execution_outcome_for_submission, Attempt, AttemptClosure, AttemptFailReason, AttemptId,
    AttemptStage, ExecutionRole, GeneratorSubmission, MaterializedPlan, PlannerFailReason,
    PlannerFailureSubmission, PlannerPlan, PlannerSubmission, ReducerSubmission, Task,
    TaskOutcomeStatus, TaskRole, TaskStatus,
};

use crate::attempt::plan_dag::{validate_plan_agents, validate_plan_shape};
use crate::attempt::{
    AgentLaunch, AgentLaunchFactory, AgentRunReport, AttemptResources, AttemptStageAdvancer,
};
use crate::ids::{generator_task_id, planner_id, planner_task_id_for, reducer_task_id};
use crate::util::json_object;
use crate::{Result, WorkflowError};

struct ExecutionMark {
    task_id: eos_types::TaskId,
    expected_role: TaskRole,
    outcome_role: ExecutionRole,
    status: TaskOutcomeStatus,
    outcome: String,
    terminal_payload: eos_types::JsonObject,
}

/// State machine for one Attempt.
pub struct AttemptOrchestrator {
    attempt_id: AttemptId,
    deps: AttemptResources,
}

impl std::fmt::Debug for AttemptOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptOrchestrator")
            .field("attempt_id", &self.attempt_id)
            .finish()
    }
}

impl AttemptOrchestrator {
    /// Create an orchestrator for `attempt`.
    #[must_use]
    pub fn new(attempt: &Attempt, deps: AttemptResources) -> Arc<Self> {
        Arc::new(Self {
            attempt_id: attempt.id.clone(),
            deps,
        })
    }

    /// Attempt id.
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    /// Start the PLAN stage by creating the planner task.
    ///
    /// # Errors
    /// Returns [`WorkflowError`] if the attempt is not startable.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.validate_run_concurrency()?;
        let attempt = self.assert_stage(AttemptStage::Plan).await?;
        if attempt.planner_task_id().is_some() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} already has a planner task",
                attempt.id.as_str()
            )));
        }
        let planner_id = planner_id();
        let task_id = planner_task_id_for(&attempt.id, &planner_id)?;
        let launch = AgentLaunchFactory::new(self.deps.clone())
            .for_planner(&attempt, planner_id, task_id.clone())
            .await?;
        self.deps.orchestrator_registry.register(Arc::clone(self))?;
        let result: Result<()> = async {
            self.deps
                .task_store
                .insert_task(&Task {
                    id: task_id.clone(),
                    request_id: launch.request_id().clone(),
                    role: TaskRole::Planner,
                    instruction: launch.context().to_owned(),
                    status: TaskStatus::Running,
                    workflow_id: Some(launch.workflow_id().clone()),
                    iteration_id: Some(attempt.iteration_id.clone()),
                    attempt_id: Some(attempt.id.clone()),
                    agent_name: Some(launch.agent_name().to_owned()),
                    needs: Vec::new(),
                    outcomes: Vec::new(),
                    terminal_payload: None,
                })
                .await?;
            self.deps
                .attempt_store
                .record_planner_task(&attempt.id, &task_id)
                .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.deps.orchestrator_registry.deregister(&attempt.id);
        }
        result?;
        self.spawn_planner_run(launch);
        Ok(())
    }

    fn spawn_planner_run(self: &Arc<Self>, launch: AgentLaunch) {
        let orchestrator = Arc::clone(self);
        let runner = self.deps.runner.clone();
        tokio::spawn(async move {
            let report = runner.run(launch.clone()).await;
            if let Err(err) = orchestrator.settle_planner(launch, report).await {
                tracing::warn!(
                    attempt_id = %orchestrator.attempt_id.as_str(),
                    error = %err,
                    "planner run could not be settled"
                );
            }
        });
    }

    /// Settle the planner run after it resolves (Path A-recording). The submit
    /// tool already recorded the plan via [`record_plan`](Self::record_plan)
    /// *during* the run (materialize + stage RUN + planner Done), so the only
    /// post-run jobs are: planner Done -> kick the single `advance_run_stage`;
    /// planner still Running (a dead/failed planner that never submitted) ->
    /// synthesize `run_exhausted` and close FAILED. This is the sole
    /// `advance_run_stage` caller (D4: exactly one writer).
    async fn settle_planner(
        self: &Arc<Self>,
        launch: AgentLaunch,
        report: Result<AgentRunReport>,
    ) -> Result<()> {
        match report {
            Ok(report) => {
                if let Some(summary) = &report.failure_summary {
                    tracing::warn!(
                        attempt_id = %self.attempt_id.as_str(),
                        task_id = %launch.task_id().as_str(),
                        %summary,
                        "planner run reported a failure summary"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    attempt_id = %self.attempt_id.as_str(),
                    task_id = %launch.task_id().as_str(),
                    error = %err,
                    "planner run failed"
                );
            }
        }
        let planner_status = self
            .deps
            .task_store
            .get(launch.task_id())
            .await?
            .map(|task| task.status);
        match planner_status {
            Some(TaskStatus::Done) => {
                AttemptStageAdvancer::new(Arc::clone(self))
                    .advance_run_stage()
                    .await
            }
            Some(TaskStatus::Failed) => Ok(()),
            _ => self.synthesize_planner_failure(&launch).await,
        }
    }

    async fn synthesize_planner_failure(&self, launch: &AgentLaunch) -> Result<()> {
        self.apply_planner_failure(PlannerFailureSubmission {
            attempt_id: launch.attempt_id().clone(),
            planner_task_id: launch.task_id().clone(),
            fail_reason: PlannerFailReason::RunExhausted,
        })
        .await
    }

    /// Record a validated planner plan from `eos-tool` (Path A-recording).
    ///
    /// Materializes the generator + reducer task rows, marks the planner Done,
    /// and sets stage RUN — but does **not** advance. The single
    /// `advance_run_stage` is kicked once by [`settle_planner`](Self::settle_planner)
    /// in the planner's spawned continuation, so the submit tool returns promptly
    /// (it does not block on the whole run stage).
    pub(crate) async fn record_plan(&self, plan: PlannerPlan) -> Result<()> {
        let persisted = self.materialize_plan_tasks(&plan).await?;
        self.record_plan_submission(persisted).await
    }

    async fn materialize_plan_tasks(&self, plan: &PlannerPlan) -> Result<PlannerSubmission> {
        validate_plan_shape(plan)?;
        let attempt = self
            .validate_planner_submission(&plan.planner_task_id)
            .await?;
        let planner_task = self
            .deps
            .task_store
            .get(&plan.planner_task_id)
            .await?
            .ok_or_else(|| {
                WorkflowError::not_found("planner task", plan.planner_task_id.as_str())
            })?;
        // Validate every plan agent (registry / D6 role / task-spec presence)
        // BEFORE writing any task row, so a rejected plan never leaves orphan
        // Pending rows. Mirrors PLAN §3 ("validate shape / acyclic / ROLE, then
        // materialize") and Rust `build_planner_submission`, which resolves all
        // agents up front before creating tasks.
        validate_plan_agents(plan, &self.deps.agent_registry)?;
        let mut generator_to_task = BTreeMap::new();
        for task in &plan.tasks {
            let id = generator_task_id(&attempt.id, &task.generator_id)?;
            generator_to_task.insert(task.generator_id.clone(), id);
        }
        let mut reducer_to_task = BTreeMap::new();
        for reducer in &plan.reducers {
            let id = reducer_task_id(&attempt.id, &reducer.reducer_id)?;
            reducer_to_task.insert(reducer.reducer_id.clone(), id);
        }

        let mut generator_ids = Vec::with_capacity(plan.tasks.len());
        for task in &plan.tasks {
            let instruction = plan
                .task_specs
                .get(&task.generator_id)
                .ok_or_else(|| WorkflowError::not_found("task spec", task.generator_id.as_str()))?
                .clone();
            let id = generator_to_task
                .get(&task.generator_id)
                .cloned()
                .ok_or_else(|| WorkflowError::not_found("generator", task.generator_id.as_str()))?;
            let needs =
                task.needs
                    .iter()
                    .map(|need| {
                        generator_to_task.get(need).cloned().ok_or_else(|| {
                            WorkflowError::not_found("generator need", need.as_str())
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
            self.deps
                .task_store
                .insert_task(&Task {
                    id: id.clone(),
                    request_id: planner_task.request_id.clone(),
                    role: TaskRole::Generator,
                    instruction,
                    status: TaskStatus::Pending,
                    workflow_id: Some(attempt.workflow_id.clone()),
                    iteration_id: Some(attempt.iteration_id.clone()),
                    attempt_id: Some(attempt.id.clone()),
                    agent_name: Some(task.agent_name.clone()),
                    needs,
                    outcomes: Vec::new(),
                    terminal_payload: None,
                })
                .await?;
            generator_ids.push(id);
        }

        let mut reducer_ids = Vec::with_capacity(plan.reducers.len());
        for reducer in &plan.reducers {
            let id = reducer_to_task
                .get(&reducer.reducer_id)
                .cloned()
                .ok_or_else(|| WorkflowError::not_found("reducer", reducer.reducer_id.as_str()))?;
            let needs = reducer
                .needs
                .iter()
                .map(|need| {
                    generator_to_task
                        .get(need)
                        .cloned()
                        .ok_or_else(|| WorkflowError::not_found("reducer need", need.as_str()))
                })
                .collect::<Result<Vec<_>>>()?;
            self.deps
                .task_store
                .insert_task(&Task {
                    id: id.clone(),
                    request_id: planner_task.request_id.clone(),
                    role: TaskRole::Reducer,
                    instruction: reducer.prompt.clone(),
                    status: TaskStatus::Pending,
                    workflow_id: Some(attempt.workflow_id.clone()),
                    iteration_id: Some(attempt.iteration_id.clone()),
                    attempt_id: Some(attempt.id.clone()),
                    agent_name: Some("reducer".to_owned()),
                    needs,
                    outcomes: Vec::new(),
                    terminal_payload: None,
                })
                .await?;
            reducer_ids.push(id);
        }

        Ok(PlannerSubmission {
            attempt_id: plan.attempt_id.clone(),
            plan: MaterializedPlan {
                planner_task_id: plan.planner_task_id.clone(),
                disposition: plan.disposition.clone(),
                generator_task_ids: generator_ids,
                reducer_task_ids: reducer_ids,
            },
        })
    }

    async fn record_plan_submission(&self, submission: PlannerSubmission) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        self.validate_run_concurrency()?;
        let attempt = self
            .validate_planner_submission(&submission.plan.planner_task_id)
            .await?;
        let planner_result = json_object("kind", submission.plan.disposition.kind_label());
        self.deps
            .task_store
            .set_task_status_if_current(
                &submission.plan.planner_task_id,
                TaskStatus::Running,
                TaskStatus::Done,
                Some(&[]),
                Some(&planner_result),
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "planner task {:?} is no longer running",
                    submission.plan.planner_task_id.as_str()
                ))
            })?;
        self.deps
            .attempt_store
            .record_plan(&attempt.id, &submission.plan)
            .await?;
        // NO advance here (Path A-recording): `settle_planner` kicks the single
        // `advance_run_stage` once the planner run resolves.
        Ok(())
    }

    /// Apply planner exhaustion.
    pub async fn apply_planner_failure(&self, submission: PlannerFailureSubmission) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        self.validate_planner_submission(&submission.planner_task_id)
            .await?;
        let planner_result = json_object(
            "fail_reason",
            match submission.fail_reason {
                PlannerFailReason::RunExhausted => "run_exhausted",
            },
        );
        self.deps
            .task_store
            .set_task_status_if_current(
                &submission.planner_task_id,
                TaskStatus::Running,
                TaskStatus::Failed,
                Some(&[]),
                Some(&planner_result),
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "planner task {:?} is no longer running",
                    submission.planner_task_id.as_str()
                ))
            })?;
        self.close_attempt_failed(AttemptFailReason::TaskFailed)
            .await
    }

    pub(crate) async fn record_generator_submission(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        let attempt = self.assert_stage(AttemptStage::Run).await?;
        if !attempt.generator_task_ids().contains(&submission.task_id) {
            return Err(WorkflowError::invariant(format!(
                "generator submission task {:?} is not a generator of attempt {:?}",
                submission.task_id.as_str(),
                attempt.id.as_str()
            )));
        }
        self.mark_execution_task(
            &attempt,
            ExecutionMark {
                task_id: submission.task_id,
                expected_role: TaskRole::Generator,
                outcome_role: ExecutionRole::Generator,
                status: submission.status,
                outcome: submission.outcome,
                terminal_payload: submission.terminal_payload,
            },
        )
        .await
    }

    pub(crate) async fn record_reducer_submission(
        &self,
        submission: ReducerSubmission,
    ) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        let attempt = self.assert_stage(AttemptStage::Run).await?;
        if !attempt.reducer_task_ids().contains(&submission.task_id) {
            return Err(WorkflowError::invariant(format!(
                "reducer submission task {:?} is not a reducer of attempt {:?}",
                submission.task_id.as_str(),
                attempt.id.as_str()
            )));
        }
        self.mark_execution_task(
            &attempt,
            ExecutionMark {
                task_id: submission.task_id,
                expected_role: TaskRole::Reducer,
                outcome_role: ExecutionRole::Reducer,
                status: submission.status,
                outcome: submission.outcome,
                terminal_payload: submission.terminal_payload,
            },
        )
        .await
    }

    async fn mark_execution_task(&self, attempt: &Attempt, mark: ExecutionMark) -> Result<()> {
        let task = self
            .deps
            .task_store
            .get(&mark.task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("task", mark.task_id.as_str()))?;
        if task.attempt_id.as_ref() != Some(&attempt.id) {
            return Err(WorkflowError::invariant(format!(
                "task {:?} does not belong to attempt {:?}",
                mark.task_id.as_str(),
                attempt.id.as_str()
            )));
        }
        if task.role != mark.expected_role {
            return Err(WorkflowError::invariant(format!(
                "task {:?} has wrong role",
                mark.task_id.as_str()
            )));
        }
        if task.status != TaskStatus::Running {
            return Err(WorkflowError::invariant(format!(
                "task {:?} is not running",
                mark.task_id.as_str()
            )));
        }
        // `TaskOutcomeStatus` is binary, so the submission status maps 1:1 to the
        // task status; the outcome carries `mark.status` directly (no round-trip).
        let task_status = match mark.status {
            TaskOutcomeStatus::Success => TaskStatus::Done,
            TaskOutcomeStatus::Failed => TaskStatus::Failed,
        };
        let result = execution_outcome_for_submission(
            mark.task_id.clone(),
            mark.outcome_role,
            mark.status,
            mark.outcome,
        );
        self.deps
            .task_store
            .set_task_status_if_current(
                &mark.task_id,
                TaskStatus::Running,
                task_status,
                Some(&[result]),
                Some(&mark.terminal_payload),
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "task {:?} is no longer running",
                    mark.task_id.as_str()
                ))
            })?;
        Ok(())
    }

    pub(crate) async fn close_attempt_passed(&self) -> Result<()> {
        self.close_attempt(AttemptClosure::Passed {
            outcomes: Vec::new(),
            closed_at: eos_types::UtcDateTime::now(),
        })
        .await
    }

    pub(crate) async fn close_attempt_failed(&self, reason: AttemptFailReason) -> Result<()> {
        self.close_attempt(AttemptClosure::Failed {
            reason,
            outcomes: Vec::new(),
            closed_at: eos_types::UtcDateTime::now(),
        })
        .await
    }

    async fn close_attempt(&self, closure: AttemptClosure) -> Result<()> {
        let attempt = self.fresh_attempt().await?;
        if attempt.is_closed() {
            return Ok(());
        }
        let outcomes =
            crate::state::project_attempt_outcomes(&attempt, Some(self.deps.task_store.as_ref()))
                .await?;
        let closure = match closure {
            AttemptClosure::Passed { closed_at, .. } => AttemptClosure::Passed {
                outcomes,
                closed_at,
            },
            AttemptClosure::Failed {
                reason, closed_at, ..
            } => AttemptClosure::Failed {
                reason,
                outcomes,
                closed_at,
            },
            AttemptClosure::Cancelled {
                reason, closed_at, ..
            } => AttemptClosure::Cancelled {
                reason,
                outcomes,
                closed_at,
            },
        };
        let closed = self.deps.attempt_store.close(&attempt.id, closure).await?;
        self.deps.orchestrator_registry.deregister(&attempt.id);
        if let Some(registry) = &self.deps.iteration_coordinators {
            if let Some(coordinator) = registry.get(&closed.iteration_id) {
                coordinator.handle_attempt_closed(&closed.id).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn plan_task_records(&self, attempt: &Attempt) -> Result<Vec<Task>> {
        let mut out = Vec::new();
        for task_id in attempt
            .generator_task_ids()
            .iter()
            .chain(attempt.reducer_task_ids().iter())
        {
            let task = self
                .deps
                .task_store
                .get(task_id)
                .await?
                .ok_or_else(|| WorkflowError::not_found("plan task", task_id.as_str()))?;
            out.push(task);
        }
        Ok(out)
    }

    pub(crate) async fn fresh_attempt(&self) -> Result<Attempt> {
        self.deps
            .attempt_store
            .get(&self.attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", self.attempt_id.as_str()))
    }

    async fn assert_stage(&self, expected: AttemptStage) -> Result<Attempt> {
        let attempt = self.fresh_attempt().await?;
        if attempt.is_closed() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} is already closed",
                attempt.id.as_str()
            )));
        }
        if attempt.stage() != expected {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} expected stage {:?}, got {:?}",
                attempt.id.as_str(),
                expected,
                attempt.stage()
            )));
        }
        Ok(attempt)
    }

    async fn validate_planner_submission(
        &self,
        planner_task_id: &eos_types::TaskId,
    ) -> Result<Attempt> {
        let attempt = self.assert_stage(AttemptStage::Plan).await?;
        if attempt.planner_task_id() != Some(planner_task_id) {
            return Err(WorkflowError::invariant(format!(
                "planner submission task {:?} does not match attempt planner",
                planner_task_id.as_str()
            )));
        }
        let task = self
            .deps
            .task_store
            .get(planner_task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("planner task", planner_task_id.as_str()))?;
        if task.attempt_id.as_ref() != Some(&attempt.id) || task.role != TaskRole::Planner {
            return Err(WorkflowError::invariant(format!(
                "task {:?} is not this attempt's planner task",
                planner_task_id.as_str()
            )));
        }
        Ok(attempt)
    }

    fn assert_submission_attempt(&self, attempt_id: &AttemptId) -> Result<()> {
        if attempt_id != &self.attempt_id {
            return Err(WorkflowError::invariant(format!(
                "submission attempt {:?} does not match orchestrator attempt {:?}",
                attempt_id.as_str(),
                self.attempt_id.as_str()
            )));
        }
        Ok(())
    }

    pub(crate) fn deps(&self) -> &AttemptResources {
        &self.deps
    }

    fn validate_run_concurrency(&self) -> Result<()> {
        if self.deps.max_concurrent_task_runs == 0 {
            return Err(WorkflowError::invariant(
                "max_concurrent_task_runs must be at least 1",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/attempt/orchestrator/mod.rs"]
mod tests;
