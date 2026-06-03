//! Shared `#[cfg(test)]` fixtures for the crate's per-module AC tests:
//! in-memory `Store` implementations, agent-runner doubles (`QueueRunner` for
//! scripted FIFO reports, `ScriptedRunner` for launch-derived reports), the
//! workflow agent registry, and small builders. The proving tests live next to
//! the code they cover (`starter::tests`, `lifecycle::tests`, etc.).
#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_agent_def::{AgentDefinition, AgentName, AgentRegistry, AgentRegistryBuilder, AgentRole};
use eos_state::{
    Attempt, AttemptFailReason, AttemptId, AttemptStage, AttemptStatus, CoreError,
    ExecutionTaskOutcome, GeneratorSubmission, Iteration, IterationCreationReason, IterationId,
    IterationStatus, JsonObject, PlannerKind, ReducerSubmission, RequestId, Task, TaskId,
    TaskOutcomeStatus, TaskRole, TaskStatus, Workflow, WorkflowId, WorkflowStatus,
};
use eos_tools::{PlanReducer, PlanSubmissionPort, PlanTask, PlannerPlan};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;

use crate::attempt::{AgentLaunch, AgentRunReport, AgentRunner, AttemptDeps, AttemptOrchestratorRegistry};
use crate::iteration::OpenIterationCoordinatorRegistry;
use crate::{PlanSubmissionAdapter, Result};

/// A scripted terminal submission a test double records via the recording
/// [`PlanSubmissionPort`] during `run()` — the same tool->record path the real
/// submit tools take (Path A-recording). Replaces the old `AgentTerminal` enum
/// the runner used to return for the loop to apply.
#[derive(Debug, Clone)]
pub(crate) enum ScriptedSubmission {
    /// The planner submits a plan (records via `apply_plan`).
    Planner(PlannerPlan),
    /// A generator submits its outcome (records via `submit_generator`).
    Generator(GeneratorSubmission),
    /// A reducer submits its outcome (records via `apply_reducer`).
    Reducer(ReducerSubmission),
    /// A dead agent: the run ends without recording, so the owning loop catches
    /// it via the still-RUNNING exhaustion guard (`run_exhausted`).
    NoSubmission(String),
}

/// Build the recording port over an attempt registry (the test analogue of the
/// production `PlanSubmissionAdapter` wiring at the composition root).
pub(crate) fn recording_port(
    registry: &Arc<AttemptOrchestratorRegistry>,
) -> Arc<dyn PlanSubmissionPort> {
    Arc::new(PlanSubmissionAdapter::new(registry.clone()))
}

#[derive(Debug, Default)]
pub(crate) struct MemoryStores {
    workflows: Mutex<HashMap<WorkflowId, Workflow>>,
    iterations: Mutex<HashMap<IterationId, Iteration>>,
    attempts: Mutex<HashMap<AttemptId, Attempt>>,
    tasks: Mutex<HashMap<TaskId, Task>>,
    /// Count of every mutating `TaskStore` call (AC-eos-workflow-05).
    task_writes: AtomicUsize,
}

impl MemoryStores {
    pub(crate) fn deps(self: &Arc<Self>, runner: Arc<dyn AgentRunner>) -> AttemptDeps {
        let store = Arc::clone(self);
        let mut deps = AttemptDeps::new(
            store.clone(),
            store.clone(),
            store.clone(),
            store,
            Arc::new(agent_registry()),
            runner,
        );
        deps.orchestrator_registry = Arc::new(AttemptOrchestratorRegistry::new());
        deps.iteration_coordinators = Some(Arc::new(OpenIterationCoordinatorRegistry::new()));
        deps.max_concurrent_task_runs = 2;
        deps
    }

    pub(crate) fn seed_task(&self, task: Task) {
        self.tasks.lock().insert(task.id.clone(), task);
    }

    pub(crate) fn task(&self, id: &TaskId) -> Option<Task> {
        self.tasks.lock().get(id).cloned()
    }

    pub(crate) fn workflow(&self, id: &WorkflowId) -> Option<Workflow> {
        self.workflows.lock().get(id).cloned()
    }

    pub(crate) fn iteration(&self, id: &IterationId) -> Option<Iteration> {
        self.iterations.lock().get(id).cloned()
    }

    pub(crate) fn attempt(&self, id: &AttemptId) -> Option<Attempt> {
        self.attempts.lock().get(id).cloned()
    }

    /// Total mutating `TaskStore` calls observed so far.
    pub(crate) fn task_write_count(&self) -> usize {
        self.task_writes.load(Ordering::Relaxed)
    }

    // Direct seeders for context/lifecycle tests (the trait `insert`/`get`
    // methods are ambiguous across the four store traits; these inherent
    // wrappers keep test bodies readable and do not touch `task_writes`).

    pub(crate) async fn seed_workflow(&self, goal: &str) -> Workflow {
        eos_state::WorkflowStore::insert(self, &RequestId::new_v4(), &tid("root"), goal)
            .await
            .unwrap()
    }

    pub(crate) async fn seed_iteration(
        &self,
        workflow_id: &WorkflowId,
        sequence_no: i64,
        creation_reason: IterationCreationReason,
        iteration_goal: &str,
        attempt_budget: i64,
    ) -> Iteration {
        let iteration = eos_state::IterationStore::insert(
            self,
            workflow_id,
            sequence_no,
            creation_reason,
            iteration_goal,
            attempt_budget,
        )
        .await
        .unwrap();
        eos_state::WorkflowStore::append_iteration_id(self, workflow_id, &iteration.id)
            .await
            .unwrap();
        iteration
    }

    pub(crate) async fn seed_attempt(
        &self,
        iteration_id: &IterationId,
        workflow_id: &WorkflowId,
        sequence_no: i64,
    ) -> Attempt {
        let attempt = eos_state::AttemptStore::insert(self, iteration_id, workflow_id, sequence_no)
            .await
            .unwrap();
        eos_state::IterationStore::append_attempt_id(self, iteration_id, &attempt.id)
            .await
            .unwrap();
        attempt
    }
}

pub(crate) fn tid(id: &str) -> TaskId {
    id.parse().unwrap()
}

impl eos_state::Sealed for MemoryStores {}

#[async_trait]
impl eos_state::WorkflowStore for MemoryStores {
    async fn insert(
        &self,
        request_id: &RequestId,
        parent_task_id: &TaskId,
        workflow_goal: &str,
    ) -> std::result::Result<Workflow, CoreError> {
        let now = eos_state::UtcDateTime::now();
        let workflow = Workflow {
            id: WorkflowId::new_v4(),
            request_id: request_id.clone(),
            workflow_goal: workflow_goal.to_owned(),
            status: WorkflowStatus::Open,
            iteration_ids: Vec::new(),
            parent_task_id: parent_task_id.clone(),
            outcomes: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        };
        self.workflows
            .lock()
            .insert(workflow.id.clone(), workflow.clone());
        Ok(workflow)
    }

    async fn get(&self, id: &WorkflowId) -> std::result::Result<Option<Workflow>, CoreError> {
        Ok(self.workflows.lock().get(id).cloned())
    }

    async fn append_iteration_id(
        &self,
        id: &WorkflowId,
        iteration_id: &IterationId,
    ) -> std::result::Result<Workflow, CoreError> {
        let mut guard = self.workflows.lock();
        let workflow = guard
            .get_mut(id)
            .ok_or_else(|| not_found("workflow", id.as_str()))?;
        workflow.iteration_ids.push(iteration_id.clone());
        workflow.updated_at = eos_state::UtcDateTime::now();
        Ok(workflow.clone())
    }

    async fn set_status(
        &self,
        id: &WorkflowId,
        status: WorkflowStatus,
        closed_at: Option<eos_state::UtcDateTime>,
        outcomes: Option<&str>,
    ) -> std::result::Result<Workflow, CoreError> {
        let mut guard = self.workflows.lock();
        let workflow = guard
            .get_mut(id)
            .ok_or_else(|| not_found("workflow", id.as_str()))?;
        workflow.status = status;
        workflow.closed_at = closed_at;
        workflow.updated_at = eos_state::UtcDateTime::now();
        if let Some(outcomes) = outcomes {
            workflow.outcomes = Some(outcomes.to_owned());
        }
        Ok(workflow.clone())
    }

    async fn list_for_parent_task(
        &self,
        parent_task_id: &TaskId,
    ) -> std::result::Result<Vec<Workflow>, CoreError> {
        let mut workflows: Vec<Workflow> = self
            .workflows
            .lock()
            .values()
            .filter(|workflow| &workflow.parent_task_id == parent_task_id)
            .cloned()
            .collect();
        workflows.sort_by_key(|workflow| workflow.created_at);
        Ok(workflows)
    }
}

#[async_trait]
impl eos_state::IterationStore for MemoryStores {
    async fn insert(
        &self,
        workflow_id: &WorkflowId,
        sequence_no: i64,
        creation_reason: IterationCreationReason,
        iteration_goal: &str,
        attempt_budget: i64,
    ) -> std::result::Result<Iteration, CoreError> {
        let now = eos_state::UtcDateTime::now();
        let iteration = Iteration {
            id: IterationId::new_v4(),
            workflow_id: workflow_id.clone(),
            sequence_no,
            creation_reason,
            iteration_goal: iteration_goal.to_owned(),
            attempt_budget,
            status: IterationStatus::Open,
            attempt_ids: Vec::new(),
            deferred_goal_for_next_iteration: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            outcomes: None,
        };
        self.iterations
            .lock()
            .insert(iteration.id.clone(), iteration.clone());
        Ok(iteration)
    }

    async fn get(&self, id: &IterationId) -> std::result::Result<Option<Iteration>, CoreError> {
        Ok(self.iterations.lock().get(id).cloned())
    }

    async fn append_attempt_id(
        &self,
        id: &IterationId,
        attempt_id: &AttemptId,
    ) -> std::result::Result<Iteration, CoreError> {
        let mut guard = self.iterations.lock();
        let iteration = guard
            .get_mut(id)
            .ok_or_else(|| not_found("iteration", id.as_str()))?;
        iteration.attempt_ids.push(attempt_id.clone());
        iteration.updated_at = eos_state::UtcDateTime::now();
        Ok(iteration.clone())
    }

    async fn set_status(
        &self,
        id: &IterationId,
        status: IterationStatus,
        closed_at: Option<eos_state::UtcDateTime>,
        outcomes: Option<&str>,
    ) -> std::result::Result<Iteration, CoreError> {
        let mut guard = self.iterations.lock();
        let iteration = guard
            .get_mut(id)
            .ok_or_else(|| not_found("iteration", id.as_str()))?;
        iteration.status = status;
        iteration.closed_at = closed_at;
        iteration.updated_at = eos_state::UtcDateTime::now();
        if let Some(outcomes) = outcomes {
            iteration.outcomes = Some(outcomes.to_owned());
        }
        Ok(iteration.clone())
    }

    async fn set_deferred_goal_for_next_iteration(
        &self,
        id: &IterationId,
        deferred_goal_for_next_iteration: Option<&str>,
    ) -> std::result::Result<Iteration, CoreError> {
        let mut guard = self.iterations.lock();
        let iteration = guard
            .get_mut(id)
            .ok_or_else(|| not_found("iteration", id.as_str()))?;
        iteration.deferred_goal_for_next_iteration =
            deferred_goal_for_next_iteration.map(ToOwned::to_owned);
        iteration.updated_at = eos_state::UtcDateTime::now();
        Ok(iteration.clone())
    }

    async fn close_succeeded(
        &self,
        id: &IterationId,
        outcomes: &str,
        closed_at: Option<eos_state::UtcDateTime>,
    ) -> std::result::Result<Iteration, CoreError> {
        self.set_status(id, IterationStatus::Succeeded, closed_at, Some(outcomes))
            .await
    }

    async fn list_for_workflow(
        &self,
        workflow_id: &WorkflowId,
    ) -> std::result::Result<Vec<Iteration>, CoreError> {
        let mut iterations: Vec<Iteration> = self
            .iterations
            .lock()
            .values()
            .filter(|iteration| &iteration.workflow_id == workflow_id)
            .cloned()
            .collect();
        iterations.sort_by_key(|iteration| iteration.sequence_no);
        Ok(iterations)
    }
}

#[async_trait]
impl eos_state::AttemptStore for MemoryStores {
    async fn insert(
        &self,
        iteration_id: &IterationId,
        workflow_id: &WorkflowId,
        attempt_sequence_no: i64,
    ) -> std::result::Result<Attempt, CoreError> {
        let now = eos_state::UtcDateTime::now();
        let attempt = Attempt {
            id: AttemptId::new_v4(),
            iteration_id: iteration_id.clone(),
            workflow_id: workflow_id.clone(),
            attempt_sequence_no,
            stage: AttemptStage::Plan,
            status: AttemptStatus::Running,
            planner_task_id: None,
            generator_task_ids: Vec::new(),
            reducer_task_ids: Vec::new(),
            deferred_goal_for_next_iteration: None,
            fail_reason: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            outcomes: Vec::new(),
        };
        self.attempts
            .lock()
            .insert(attempt.id.clone(), attempt.clone());
        Ok(attempt)
    }

    async fn get(&self, id: &AttemptId) -> std::result::Result<Option<Attempt>, CoreError> {
        Ok(self.attempts.lock().get(id).cloned())
    }

    async fn set_stage(
        &self,
        id: &AttemptId,
        stage: AttemptStage,
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.stage = stage;
        attempt.updated_at = eos_state::UtcDateTime::now();
        Ok(attempt.clone())
    }

    async fn set_planner_task_id(
        &self,
        id: &AttemptId,
        planner_task_id: &TaskId,
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.planner_task_id = Some(planner_task_id.clone());
        attempt.updated_at = eos_state::UtcDateTime::now();
        Ok(attempt.clone())
    }

    async fn set_generator_task_ids(
        &self,
        id: &AttemptId,
        generator_task_ids: &[TaskId],
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.generator_task_ids = generator_task_ids.to_vec();
        attempt.updated_at = eos_state::UtcDateTime::now();
        Ok(attempt.clone())
    }

    async fn set_reducer_task_ids(
        &self,
        id: &AttemptId,
        reducer_task_ids: &[TaskId],
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.reducer_task_ids = reducer_task_ids.to_vec();
        attempt.updated_at = eos_state::UtcDateTime::now();
        Ok(attempt.clone())
    }

    async fn set_deferred_goal(
        &self,
        id: &AttemptId,
        deferred_goal_for_next_iteration: Option<&str>,
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.deferred_goal_for_next_iteration =
            deferred_goal_for_next_iteration.map(ToOwned::to_owned);
        attempt.updated_at = eos_state::UtcDateTime::now();
        Ok(attempt.clone())
    }

    async fn close(
        &self,
        id: &AttemptId,
        status: AttemptStatus,
        fail_reason: Option<AttemptFailReason>,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        closed_at: eos_state::UtcDateTime,
    ) -> std::result::Result<Attempt, CoreError> {
        let mut guard = self.attempts.lock();
        let attempt = guard
            .get_mut(id)
            .ok_or_else(|| not_found("attempt", id.as_str()))?;
        attempt.stage = AttemptStage::Closed;
        attempt.status = status;
        attempt.fail_reason = fail_reason;
        attempt.closed_at = Some(closed_at);
        attempt.updated_at = eos_state::UtcDateTime::now();
        if let Some(outcomes) = outcomes {
            attempt.outcomes = outcomes.to_vec();
        }
        Ok(attempt.clone())
    }

    async fn list_for_iteration(
        &self,
        iteration_id: &IterationId,
    ) -> std::result::Result<Vec<Attempt>, CoreError> {
        let mut attempts: Vec<Attempt> = self
            .attempts
            .lock()
            .values()
            .filter(|attempt| &attempt.iteration_id == iteration_id)
            .cloned()
            .collect();
        attempts.sort_by_key(|attempt| attempt.attempt_sequence_no);
        Ok(attempts)
    }
}

#[async_trait]
impl eos_state::TaskStore for MemoryStores {
    async fn upsert_task(&self, task: &Task) -> std::result::Result<(), CoreError> {
        self.task_writes.fetch_add(1, Ordering::Relaxed);
        self.tasks.lock().insert(task.id.clone(), task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> std::result::Result<Option<Task>, CoreError> {
        Ok(self.tasks.lock().get(id).cloned())
    }

    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> std::result::Result<Task, CoreError> {
        self.task_writes.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.tasks.lock();
        let task = guard
            .get_mut(id)
            .ok_or_else(|| not_found("task", id.as_str()))?;
        update_task(task, status, outcomes, terminal_tool_result);
        Ok(task.clone())
    }

    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> std::result::Result<Option<Task>, CoreError> {
        self.task_writes.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.tasks.lock();
        let task = guard
            .get_mut(id)
            .ok_or_else(|| not_found("task", id.as_str()))?;
        if task.status != expected {
            return Ok(None);
        }
        update_task(task, status, outcomes, terminal_tool_result);
        Ok(Some(task.clone()))
    }
}

/// Agent-runner double serving pre-pushed submissions FIFO, each recorded via
/// the bound recording port (the real tool->record path). Use for sequential,
/// single-attempt scenarios where the task ids are known after `start()`. An
/// empty queue blocks the run (the agent stays "running") until a submission is
/// pushed — used by tests that hold a planner open while exercising cancel.
#[derive(Default)]
pub(crate) struct QueueRunner {
    submissions: Mutex<VecDeque<ScriptedSubmission>>,
    launches: Mutex<Vec<AgentLaunch>>,
    port: OnceLock<Arc<dyn PlanSubmissionPort>>,
    notify: Notify,
}

impl std::fmt::Debug for QueueRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueRunner").finish_non_exhaustive()
    }
}

impl QueueRunner {
    /// Bind the recording port to the attempt registry (call right after
    /// `deps()`, before the registry is moved into the starter/lifecycle).
    pub(crate) fn bind(&self, registry: &Arc<AttemptOrchestratorRegistry>) {
        let _ = self.port.set(recording_port(registry));
    }

    fn port(&self) -> &Arc<dyn PlanSubmissionPort> {
        self.port
            .get()
            .expect("QueueRunner recording port bound (call bind() after deps())")
    }

    pub(crate) fn push(&self, submission: ScriptedSubmission) {
        self.submissions.lock().push_back(submission);
        self.notify.notify_one();
    }

    pub(crate) fn launches(&self) -> Vec<AgentLaunch> {
        self.launches.lock().clone()
    }
}

#[async_trait]
impl AgentRunner for QueueRunner {
    async fn run(&self, launch: AgentLaunch) -> Result<AgentRunReport> {
        self.launches.lock().push(launch);
        let submission = loop {
            if let Some(submission) = self.submissions.lock().pop_front() {
                break submission;
            }
            self.notify.notified().await;
        };
        record_scripted(self.port(), submission).await
    }
}

/// Record a scripted submission via the recording port (the same path the real
/// submit tools take). A `NoSubmission` records nothing, so the owning loop's
/// still-RUNNING guard synthesizes `run_exhausted`.
async fn record_scripted(
    port: &Arc<dyn PlanSubmissionPort>,
    submission: ScriptedSubmission,
) -> Result<AgentRunReport> {
    match submission {
        ScriptedSubmission::NoSubmission(summary) => Ok(AgentRunReport::failed(summary)),
        ScriptedSubmission::Planner(plan) => {
            port.apply_plan(plan).await.expect("record plan via port");
            Ok(AgentRunReport::ok())
        }
        ScriptedSubmission::Generator(submission) => {
            port.submit_generator(submission)
                .await
                .expect("record generator via port");
            Ok(AgentRunReport::ok())
        }
        ScriptedSubmission::Reducer(submission) => {
            port.apply_reducer(submission)
                .await
                .expect("record reducer via port");
            Ok(AgentRunReport::ok())
        }
    }
}

/// Agent-runner double that synthesizes a role-appropriate report from each
/// `AgentLaunch`. Needed when task/attempt ids are not known up front:
/// concurrent fan-out (AC-08b) and retries that mint new attempt ids (AC-10).
pub(crate) struct ScriptedRunner {
    generators: usize,
    reducer_status: TaskOutcomeStatus,
    deferred_goal: String,
    defers_remaining: AtomicUsize,
    launches: Mutex<Vec<AgentLaunch>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    port: OnceLock<Arc<dyn PlanSubmissionPort>>,
}

impl std::fmt::Debug for ScriptedRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedRunner")
            .field("generators", &self.generators)
            .finish_non_exhaustive()
    }
}

impl ScriptedRunner {
    /// `generators` generator tasks plus one reducer needing all of them.
    /// `defers` planner runs emit a partial (`Defers`) plan carrying
    /// `deferred_goal`; later runs complete. Reducers report `reducer_status`.
    pub(crate) fn new(
        generators: usize,
        reducer_status: TaskOutcomeStatus,
        defers: usize,
        deferred_goal: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            generators,
            reducer_status,
            deferred_goal: deferred_goal.to_owned(),
            defers_remaining: AtomicUsize::new(defers),
            launches: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            port: OnceLock::new(),
        })
    }

    /// Bind the recording port to the attempt registry (call right after
    /// `deps()`, before the registry is moved into the starter).
    pub(crate) fn bind(&self, registry: &Arc<AttemptOrchestratorRegistry>) {
        let _ = self.port.set(recording_port(registry));
    }

    fn port(&self) -> &Arc<dyn PlanSubmissionPort> {
        self.port
            .get()
            .expect("ScriptedRunner recording port bound (call bind() after deps())")
    }

    pub(crate) fn launches(&self) -> Vec<AgentLaunch> {
        self.launches.lock().clone()
    }

    pub(crate) fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::Relaxed)
    }

    fn enter(&self) {
        let n = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_in_flight.fetch_max(n, Ordering::Relaxed);
    }

    fn exit(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    fn build_plan(&self, launch: &AgentLaunch) -> PlannerPlan {
        let defer = self.defers_remaining.load(Ordering::Relaxed) > 0;
        if defer {
            self.defers_remaining.fetch_sub(1, Ordering::Relaxed);
        }
        let tasks = (0..self.generators)
            .map(|i| PlanTask {
                id: format!("g{i}"),
                agent_name: "coder".to_owned(),
                needs: Vec::new(),
            })
            .collect();
        let task_specs = (0..self.generators)
            .map(|i| (format!("g{i}"), format!("do work {i}")))
            .collect();
        let reducer_needs = (0..self.generators).map(|i| format!("g{i}")).collect();
        PlannerPlan {
            attempt_id: launch
                .attempt_id
                .clone()
                .expect("planner launch attempt id"),
            planner_task_id: launch.task_id.clone(),
            kind: if defer {
                PlannerKind::Defers
            } else {
                PlannerKind::Completes
            },
            deferred_goal_for_next_iteration: defer.then(|| self.deferred_goal.clone()),
            tasks,
            task_specs,
            reducers: vec![PlanReducer {
                id: "r1".to_owned(),
                needs: reducer_needs,
                prompt: "reduce".to_owned(),
            }],
        }
    }
}

#[async_trait]
impl AgentRunner for ScriptedRunner {
    async fn run(&self, launch: AgentLaunch) -> Result<AgentRunReport> {
        self.launches.lock().push(launch.clone());
        let attempt_id = launch.attempt_id.clone().expect("launch attempt id");
        let submission = match launch.role {
            AgentRole::Planner => ScriptedSubmission::Planner(self.build_plan(&launch)),
            AgentRole::Generator => {
                self.enter();
                for _ in 0..4 {
                    tokio::task::yield_now().await;
                }
                self.exit();
                ScriptedSubmission::Generator(GeneratorSubmission {
                    attempt_id,
                    task_id: launch.task_id.clone(),
                    status: TaskOutcomeStatus::Success,
                    outcome: "generated".to_owned(),
                    terminal_tool_result: terminal_result(),
                })
            }
            AgentRole::Reducer => {
                self.enter();
                tokio::task::yield_now().await;
                self.exit();
                ScriptedSubmission::Reducer(ReducerSubmission {
                    attempt_id,
                    task_id: launch.task_id.clone(),
                    status: self.reducer_status,
                    outcome: "reduced".to_owned(),
                    terminal_tool_result: terminal_result(),
                })
            }
            other => panic!("ScriptedRunner does not serve role {other:?}"),
        };
        record_scripted(self.port(), submission).await
    }
}

fn update_task(
    task: &mut Task,
    status: TaskStatus,
    outcomes: Option<&[ExecutionTaskOutcome]>,
    terminal_tool_result: Option<&JsonObject>,
) {
    task.status = status;
    if let Some(outcomes) = outcomes {
        task.outcomes = outcomes.to_vec();
    }
    if let Some(result) = terminal_tool_result {
        task.terminal_tool_result = Some(result.clone());
    }
}

fn not_found(entity: &str, id: &str) -> CoreError {
    CoreError::Store(format!("{entity} {id} not found"))
}

fn agent_registry() -> AgentRegistry {
    let mut builder = AgentRegistryBuilder::new();
    for (name, role, terminals) in [
        ("root", AgentRole::Root, vec!["submit_root_outcome"]),
        ("planner", AgentRole::Planner, vec!["submit_plan"]),
        (
            "coder",
            AgentRole::Generator,
            vec!["submit_generator_outcome"],
        ),
        (
            "reducer",
            AgentRole::Reducer,
            vec!["submit_reducer_outcome"],
        ),
    ] {
        builder.add(agent_def(name, role, terminals));
    }
    builder.build()
}

/// A registry missing the `planner` profile, used to force a launch failure in
/// `WorkflowStarter` (AC-eos-workflow-03 compensation saga).
pub(crate) fn agent_registry_without_planner() -> AgentRegistry {
    let mut builder = AgentRegistryBuilder::new();
    for (name, role, terminals) in [
        ("root", AgentRole::Root, vec!["submit_root_outcome"]),
        (
            "coder",
            AgentRole::Generator,
            vec!["submit_generator_outcome"],
        ),
        (
            "reducer",
            AgentRole::Reducer,
            vec!["submit_reducer_outcome"],
        ),
    ] {
        builder.add(agent_def(name, role, terminals));
    }
    builder.build()
}

fn agent_def(name: &str, role: AgentRole, terminals: Vec<&str>) -> AgentDefinition {
    AgentDefinition {
        name: AgentName::new(name).unwrap(),
        description: format!("{name} agent"),
        system_prompt: None,
        model: None,
        tool_call_limit: NonZeroU32::new(16).unwrap(),
        role,
        agent_type: eos_agent_def::AgentType::Agent,
        allowed_tools: Vec::new(),
        terminals: terminals.into_iter().map(ToOwned::to_owned).collect(),
        notification_triggers: Vec::new(),
        skill: None,
        context_recipe: Some(role.as_str().to_owned()),
    }
}

pub(crate) fn root_task(id: &str, status: TaskStatus) -> Task {
    Task {
        id: id.parse().unwrap(),
        request_id: RequestId::new_v4(),
        role: TaskRole::Root,
        instruction: "root".to_owned(),
        status,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        agent_name: Some("root".to_owned()),
        needs: Vec::new(),
        outcomes: Vec::new(),
        terminal_tool_result: None,
    }
}

pub(crate) fn terminal_result() -> JsonObject {
    json!({"ok": true}).as_object().unwrap().clone()
}

/// A full one-generator/one-reducer plan keyed to a started workflow's attempt.
pub(crate) fn one_step_plan(started: &crate::StartedWorkflow) -> PlannerPlan {
    PlannerPlan {
        attempt_id: started.attempt_id.clone(),
        planner_task_id: crate::planner_task_id(&started.attempt_id).unwrap(),
        kind: PlannerKind::Completes,
        deferred_goal_for_next_iteration: None,
        tasks: vec![PlanTask {
            id: "g1".to_owned(),
            agent_name: "coder".to_owned(),
            needs: Vec::new(),
        }],
        task_specs: [("g1".to_owned(), "do work".to_owned())]
            .into_iter()
            .collect(),
        reducers: vec![PlanReducer {
            id: "r1".to_owned(),
            needs: vec!["g1".to_owned()],
            prompt: "reduce".to_owned(),
        }],
    }
}

/// Spin the runtime until `workflow_id` reaches `status`, or panic.
pub(crate) async fn wait_for_workflow_status(
    stores: &MemoryStores,
    workflow_id: &WorkflowId,
    status: WorkflowStatus,
) {
    for _ in 0..5000 {
        if stores.workflow(workflow_id).unwrap().status == status {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("workflow {workflow_id} did not reach {status:?}");
}
