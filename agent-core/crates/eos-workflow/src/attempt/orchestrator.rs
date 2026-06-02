use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use eos_agent_def::AgentName;
use eos_state::{
    execution_outcome_for_submission, Attempt, AttemptFailReason, AttemptId, AttemptStage,
    AttemptStatus, ExecutionRole, GeneratorSubmission, PlannerFailReason, PlannerFailureSubmission,
    PlannerKind, PlannerSubmission, ReducerSubmission, Task, TaskOutcomeStatus, TaskRole,
    TaskStatus,
};
use eos_tools::PlannerPlan;
use serde_json::json;

use crate::attempt::{AgentLaunchFactory, AttemptDeps, AttemptStageAdvancer};
use crate::ids::{generator_task_id, planner_task_id, reducer_task_id};
use crate::{Result, WorkflowError};

struct ExecutionMark {
    task_id: eos_state::TaskId,
    expected_role: TaskRole,
    outcome_role: ExecutionRole,
    status: TaskOutcomeStatus,
    outcome: String,
    terminal_tool_result: eos_state::JsonObject,
}

/// State machine for one Attempt.
pub struct AttemptOrchestrator {
    attempt_id: AttemptId,
    deps: AttemptDeps,
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
    pub fn new(attempt: &Attempt, deps: AttemptDeps) -> Arc<Self> {
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
        let attempt = self.assert_stage(AttemptStage::Plan).await?;
        if attempt.status != AttemptStatus::Running {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} is not running",
                attempt.id.as_str()
            )));
        }
        if attempt.planner_task_id.is_some() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} already has a planner task",
                attempt.id.as_str()
            )));
        }
        let task_id = planner_task_id(&attempt.id)?;
        self.deps.orchestrator_registry.register(Arc::clone(self))?;
        let launch = AgentLaunchFactory::new(self.deps.clone())
            .for_planner(&attempt, task_id.clone())
            .await?;
        self.deps
            .task_store
            .upsert_task(&Task {
                id: task_id.clone(),
                request_id: launch.request_id,
                role: TaskRole::Planner,
                instruction: launch.context,
                status: TaskStatus::Running,
                workflow_id: launch.workflow_id,
                iteration_id: Some(attempt.iteration_id.clone()),
                attempt_id: Some(attempt.id.clone()),
                agent_name: Some(launch.agent_name),
                needs: Vec::new(),
                outcomes: Vec::new(),
                terminal_tool_result: None,
            })
            .await?;
        self.deps
            .attempt_store
            .set_planner_task_id(&attempt.id, &task_id)
            .await?;
        Ok(())
    }

    /// Apply a rich planner plan from `eos-tools`.
    pub async fn apply_plan(self: &Arc<Self>, plan: PlannerPlan) -> Result<()> {
        let persisted = self.materialize_plan_tasks(&plan).await?;
        self.apply_plan_submission(persisted).await
    }

    async fn materialize_plan_tasks(&self, plan: &PlannerPlan) -> Result<PlannerSubmission> {
        self.validate_plan_shape(plan)?;
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
        let mut local_to_task = BTreeMap::new();
        for task in &plan.tasks {
            let id = generator_task_id(&attempt.id, &task.id)?;
            local_to_task.insert(task.id.clone(), id);
        }
        for reducer in &plan.reducers {
            let id = reducer_task_id(&attempt.id, &reducer.id)?;
            local_to_task.insert(reducer.id.clone(), id);
        }

        let mut generator_ids = Vec::with_capacity(plan.tasks.len());
        for task in &plan.tasks {
            let id = local_to_task
                .get(&task.id)
                .ok_or_else(|| WorkflowError::not_found("plan task", &task.id))?
                .clone();
            let needs = task
                .needs
                .iter()
                .map(|need| {
                    local_to_task
                        .get(need)
                        .cloned()
                        .ok_or_else(|| WorkflowError::not_found("plan need", need))
                })
                .collect::<Result<Vec<_>>>()?;
            let agent_name = AgentName::new(task.agent_name.clone())?;
            self.deps.agent_registry.get(&agent_name).ok_or_else(|| {
                WorkflowError::AgentDefinition(format!(
                    "agent definition {:?} is not registered",
                    task.agent_name
                ))
            })?;
            let instruction = plan
                .task_specs
                .get(&task.id)
                .ok_or_else(|| WorkflowError::not_found("task spec", &task.id))?
                .clone();
            self.deps
                .task_store
                .upsert_task(&Task {
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
                    terminal_tool_result: None,
                })
                .await?;
            generator_ids.push(id);
        }

        let mut reducer_ids = Vec::with_capacity(plan.reducers.len());
        let reducer_name = AgentName::new("reducer")?;
        self.deps.agent_registry.get(&reducer_name).ok_or_else(|| {
            WorkflowError::AgentDefinition(
                "agent definition \"reducer\" is not registered".to_owned(),
            )
        })?;
        for reducer in &plan.reducers {
            let id = local_to_task
                .get(&reducer.id)
                .ok_or_else(|| WorkflowError::not_found("reducer", &reducer.id))?
                .clone();
            let needs = reducer
                .needs
                .iter()
                .map(|need| {
                    local_to_task
                        .get(need)
                        .cloned()
                        .ok_or_else(|| WorkflowError::not_found("plan need", need))
                })
                .collect::<Result<Vec<_>>>()?;
            self.deps
                .task_store
                .upsert_task(&Task {
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
                    terminal_tool_result: None,
                })
                .await?;
            reducer_ids.push(id);
        }

        Ok(PlannerSubmission {
            attempt_id: plan.attempt_id.clone(),
            planner_task_id: plan.planner_task_id.clone(),
            kind: plan.kind,
            generator_task_ids: generator_ids,
            reducer_task_ids: reducer_ids,
            deferred_goal_for_next_iteration: plan.deferred_goal_for_next_iteration.clone(),
        })
    }

    fn validate_plan_shape(&self, plan: &PlannerPlan) -> Result<()> {
        if plan.reducers.is_empty() {
            return Err(WorkflowError::invariant(
                "plan must contain at least one reducer",
            ));
        }
        let generator_ids: BTreeSet<&str> =
            plan.tasks.iter().map(|task| task.id.as_str()).collect();
        let reducer_ids: BTreeSet<&str> =
            plan.reducers.iter().map(|task| task.id.as_str()).collect();
        let all_ids: BTreeSet<&str> = generator_ids.union(&reducer_ids).copied().collect();
        for task in &plan.tasks {
            let reducer_needs: Vec<&str> = task
                .needs
                .iter()
                .map(String::as_str)
                .filter(|need| reducer_ids.contains(*need))
                .collect();
            if !reducer_needs.is_empty() {
                return Err(WorkflowError::invariant(format!(
                    "generator task {:?} cannot need reducer task(s): {reducer_needs:?}",
                    task.id
                )));
            }
            for need in &task.needs {
                if !all_ids.contains(need.as_str()) {
                    return Err(WorkflowError::invariant(format!(
                        "plan task {:?} has unknown needs: {:?}",
                        task.id, need
                    )));
                }
            }
        }
        let mut downstream_by_generator: BTreeMap<&str, Vec<&str>> =
            generator_ids.iter().map(|id| (*id, Vec::new())).collect();
        for task in &plan.tasks {
            for need in &task.needs {
                if let Some(downstream) = downstream_by_generator.get_mut(need.as_str()) {
                    downstream.push(task.id.as_str());
                }
            }
        }
        for reducer in &plan.reducers {
            if reducer.needs.is_empty() {
                return Err(WorkflowError::invariant(format!(
                    "reducer task {:?} must need at least one generator",
                    reducer.id
                )));
            }
            for need in &reducer.needs {
                if reducer_ids.contains(need.as_str()) {
                    return Err(WorkflowError::invariant(format!(
                        "reducer task {:?} cannot need reducer task(s)",
                        reducer.id
                    )));
                }
                if !all_ids.contains(need.as_str()) {
                    return Err(WorkflowError::invariant(format!(
                        "plan task {:?} has unknown needs: {:?}",
                        reducer.id, need
                    )));
                }
                if let Some(downstream) = downstream_by_generator.get_mut(need.as_str()) {
                    downstream.push(reducer.id.as_str());
                }
            }
        }
        let dangling: Vec<&str> = downstream_by_generator
            .iter()
            .filter_map(|(id, downstream)| downstream.is_empty().then_some(*id))
            .collect();
        if !dangling.is_empty() {
            return Err(WorkflowError::invariant(format!(
                "plan has generator(s) no downstream task needs: {dangling:?}"
            )));
        }
        assert_acyclic(plan)
    }

    async fn apply_plan_submission(self: &Arc<Self>, submission: PlannerSubmission) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        match submission.kind {
            PlannerKind::Completes if submission.deferred_goal_for_next_iteration.is_some() => {
                return Err(WorkflowError::invariant(
                    "full plans cannot set deferred_goal_for_next_iteration",
                ));
            }
            PlannerKind::Defers if submission.deferred_goal_for_next_iteration.is_none() => {
                return Err(WorkflowError::invariant(
                    "partial plans require deferred_goal_for_next_iteration",
                ));
            }
            _ => {}
        }
        let attempt = self
            .validate_planner_submission(&submission.planner_task_id)
            .await?;
        self.deps
            .task_store
            .set_task_status(
                &submission.planner_task_id,
                TaskStatus::Done,
                Some(&[]),
                Some(
                    &json!({"kind": match submission.kind {
                        PlannerKind::Completes => "completes",
                        PlannerKind::Defers => "defers",
                    }})
                    .as_object()
                    .expect("object")
                    .clone(),
                ),
            )
            .await?;
        self.deps
            .attempt_store
            .set_deferred_goal(
                &attempt.id,
                submission.deferred_goal_for_next_iteration.as_deref(),
            )
            .await?;
        self.deps
            .attempt_store
            .set_generator_task_ids(&attempt.id, &submission.generator_task_ids)
            .await?;
        self.deps
            .attempt_store
            .set_reducer_task_ids(&attempt.id, &submission.reducer_task_ids)
            .await?;
        self.deps
            .attempt_store
            .set_stage(&attempt.id, AttemptStage::Run)
            .await?;
        AttemptStageAdvancer::new(Arc::clone(self))
            .advance_run_stage()
            .await
    }

    /// Apply planner exhaustion.
    pub async fn apply_planner_failure(&self, submission: PlannerFailureSubmission) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        self.validate_planner_submission(&submission.planner_task_id)
            .await?;
        self.deps
            .task_store
            .set_task_status(
                &submission.planner_task_id,
                TaskStatus::Failed,
                Some(&[]),
                Some(
                    &json!({"fail_reason": match submission.fail_reason {
                        PlannerFailReason::RunExhausted => "run_exhausted",
                    }})
                    .as_object()
                    .expect("object")
                    .clone(),
                ),
            )
            .await?;
        self.close_attempt(AttemptStatus::Failed, Some(AttemptFailReason::TaskFailed))
            .await
    }

    /// Apply generator submission and advance the run stage.
    pub async fn apply_generator_submission(
        self: &Arc<Self>,
        submission: GeneratorSubmission,
    ) -> Result<()> {
        self.record_generator_submission(submission).await?;
        AttemptStageAdvancer::new(Arc::clone(self))
            .advance_run_stage()
            .await
    }

    /// Apply reducer submission and advance the run stage.
    pub async fn apply_reducer_submission(
        self: &Arc<Self>,
        submission: ReducerSubmission,
    ) -> Result<()> {
        self.record_reducer_submission(submission).await?;
        AttemptStageAdvancer::new(Arc::clone(self))
            .advance_run_stage()
            .await
    }

    pub(crate) async fn record_generator_submission(
        &self,
        submission: GeneratorSubmission,
    ) -> Result<()> {
        self.assert_submission_attempt(&submission.attempt_id)?;
        let attempt = self.assert_stage(AttemptStage::Run).await?;
        if !attempt.generator_task_ids.contains(&submission.task_id) {
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
                terminal_tool_result: submission.terminal_tool_result,
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
        if !attempt.reducer_task_ids.contains(&submission.task_id) {
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
                terminal_tool_result: submission.terminal_tool_result,
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
        let task_status = if mark.status == TaskOutcomeStatus::Success {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        let execution_status = if task_status == TaskStatus::Done {
            TaskOutcomeStatus::Success
        } else {
            TaskOutcomeStatus::Failed
        };
        let result = execution_outcome_for_submission(
            mark.task_id.clone(),
            mark.outcome_role,
            execution_status,
            mark.outcome,
        );
        self.deps
            .task_store
            .set_task_status(
                &mark.task_id,
                task_status,
                Some(&[result]),
                Some(&mark.terminal_tool_result),
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn close_attempt(
        &self,
        status: AttemptStatus,
        fail_reason: Option<AttemptFailReason>,
    ) -> Result<()> {
        if status == AttemptStatus::Failed && fail_reason.is_none() {
            return Err(WorkflowError::invariant(
                "failed attempt close requires fail_reason",
            ));
        }
        if status == AttemptStatus::Passed && fail_reason.is_some() {
            return Err(WorkflowError::invariant(
                "passed attempt close cannot have fail_reason",
            ));
        }
        if status == AttemptStatus::Running {
            return Err(WorkflowError::invariant(
                "cannot close attempt with running status",
            ));
        }
        let attempt = self.fresh_attempt().await?;
        if attempt.is_closed() {
            return Ok(());
        }
        let outcomes =
            eos_state::project_attempt_outcomes(&attempt, Some(self.deps.task_store.as_ref()))
                .await?;
        let closed = self
            .deps
            .attempt_store
            .close(
                &attempt.id,
                status,
                fail_reason,
                Some(&outcomes),
                eos_state::UtcDateTime::now(),
            )
            .await?;
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
            .generator_task_ids
            .iter()
            .chain(attempt.reducer_task_ids.iter())
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
        if attempt.stage != expected {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} expected stage {:?}, got {:?}",
                attempt.id.as_str(),
                expected,
                attempt.stage
            )));
        }
        Ok(attempt)
    }

    async fn validate_planner_submission(
        &self,
        planner_task_id: &eos_state::TaskId,
    ) -> Result<Attempt> {
        let attempt = self.assert_stage(AttemptStage::Plan).await?;
        if attempt.planner_task_id.as_ref() != Some(planner_task_id) {
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

    pub(crate) fn deps(&self) -> &AttemptDeps {
        &self.deps
    }
}

fn assert_acyclic(plan: &PlannerPlan) -> Result<()> {
    let mut by_needs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for task in &plan.tasks {
        by_needs.insert(
            task.id.as_str(),
            task.needs.iter().map(String::as_str).collect(),
        );
    }
    for reducer in &plan.reducers {
        by_needs.insert(
            reducer.id.as_str(),
            reducer.needs.iter().map(String::as_str).collect(),
        );
    }
    let mut remaining = by_needs
        .iter()
        .map(|(id, needs)| (*id, needs.iter().copied().collect::<BTreeSet<_>>()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents: BTreeMap<&str, Vec<&str>> =
        by_needs.keys().map(|id| (*id, Vec::new())).collect();
    for (id, needs) in &by_needs {
        for need in needs {
            if let Some(entries) = dependents.get_mut(need) {
                entries.push(id);
            }
        }
    }
    let mut ready = remaining
        .iter()
        .filter_map(|(id, needs)| needs.is_empty().then_some(*id))
        .collect::<VecDeque<_>>();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_front() {
        order.push(id);
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(needs) = remaining.get_mut(dependent) {
                needs.remove(id);
                if needs.is_empty() {
                    ready.push_back(dependent);
                }
            }
        }
    }
    if order.len() != by_needs.len() {
        let ordered = order.into_iter().collect::<BTreeSet<_>>();
        let cycle = by_needs
            .keys()
            .filter(|id| !ordered.contains(**id))
            .copied()
            .collect::<Vec<_>>();
        return Err(WorkflowError::invariant(format!(
            "plan contains a dependency cycle among: {cycle:?}"
        )));
    }
    Ok(())
}
