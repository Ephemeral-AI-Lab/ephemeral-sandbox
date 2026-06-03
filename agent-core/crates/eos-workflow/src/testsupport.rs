#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentDefinition, AgentName, AgentRegistry, AgentRegistryBuilder, AgentRole};
use eos_state::{
    Attempt, AttemptFailReason, AttemptId, AttemptStage, AttemptStatus, CoreError,
    ExecutionTaskOutcome, GeneratorSubmission, Iteration, IterationCreationReason, IterationId,
    IterationStatus, JsonObject, ReducerSubmission, RequestId, Task, TaskId, TaskRole, TaskStatus,
    Workflow, WorkflowId, WorkflowStatus,
};
use eos_tools::WorkflowControlPort;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;

use crate::attempt::{
    AgentLaunch, AgentRunReport, AgentRunner, AgentTerminal, AttemptDeps,
    AttemptOrchestratorRegistry,
};
use crate::ids::{generator_task_id, reducer_task_id};
use crate::iteration::OpenIterationCoordinatorRegistry;
use crate::{Result, WorkflowControlAdapter, WorkflowStarter};

#[derive(Debug, Default)]
pub(crate) struct MemoryStores {
    workflows: Mutex<HashMap<WorkflowId, Workflow>>,
    iterations: Mutex<HashMap<IterationId, Iteration>>,
    attempts: Mutex<HashMap<AttemptId, Attempt>>,
    tasks: Mutex<HashMap<TaskId, Task>>,
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

#[derive(Debug, Default)]
pub(crate) struct QueueRunner {
    reports: Mutex<VecDeque<AgentRunReport>>,
    launches: Mutex<Vec<AgentLaunch>>,
    notify: Notify,
}

impl QueueRunner {
    pub(crate) fn push(&self, report: AgentRunReport) {
        self.reports.lock().push_back(report);
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
        loop {
            if let Some(report) = self.reports.lock().pop_front() {
                return Ok(report);
            }
            self.notify.notified().await;
        }
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

fn root_task(id: &str, status: TaskStatus) -> Task {
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

fn terminal_result() -> JsonObject {
    json!({"ok": true}).as_object().unwrap().clone()
}

fn one_step_plan(started: &crate::StartedWorkflow) -> eos_tools::PlannerPlan {
    eos_tools::PlannerPlan {
        attempt_id: started.attempt_id.clone(),
        planner_task_id: crate::planner_task_id(&started.attempt_id).unwrap(),
        kind: eos_state::PlannerKind::Completes,
        deferred_goal_for_next_iteration: None,
        tasks: vec![eos_tools::PlanTask {
            id: "g1".to_owned(),
            agent_name: "coder".to_owned(),
            needs: Vec::new(),
        }],
        task_specs: [("g1".to_owned(), "do work".to_owned())]
            .into_iter()
            .collect(),
        reducers: vec![eos_tools::PlanReducer {
            id: "r1".to_owned(),
            needs: vec!["g1".to_owned()],
            prompt: "reduce".to_owned(),
        }],
    }
}

async fn wait_for_workflow_status(
    stores: &MemoryStores,
    workflow_id: &WorkflowId,
    status: WorkflowStatus,
) {
    for _ in 0..100 {
        if stores.workflow(workflow_id).unwrap().status == status {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("workflow {workflow_id} did not reach {status:?}");
}

#[tokio::test]
async fn starter_creates_delegated_workflow_without_mutating_parent() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let deps = stores.deps(runner);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());

    let started = WorkflowStarter::new(deps)
        .start(" delegated goal ", &parent.id)
        .await
        .unwrap();

    assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
    assert_eq!(
        stores
            .workflow(&started.workflow_id)
            .unwrap()
            .parent_task_id,
        parent.id
    );
    assert!(stores.iteration(&started.iteration_id).unwrap().is_open());
    let attempt = stores.attempt(&started.attempt_id).unwrap();
    assert_eq!(attempt.stage, AttemptStage::Plan);
    assert_eq!(attempt.status, AttemptStatus::Running);
    assert!(stores
        .task(&crate::planner_task_id(&attempt.id).unwrap())
        .is_some());
}

#[tokio::test]
async fn reducer_success_closes_attempt_iteration_and_workflow() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let mut deps = stores.deps(runner.clone());
    deps.lifecycle_config.default_attempt_budget = 1;
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps.clone())
        .start("delegated goal", &parent.id)
        .await
        .unwrap();
    let generator_id = generator_task_id(&started.attempt_id, "g1").unwrap();
    let reducer_id = reducer_task_id(&started.attempt_id, "r1").unwrap();
    runner.push(AgentRunReport::terminal(AgentTerminal::Planner(
        one_step_plan(&started),
    )));
    runner.push(AgentRunReport::terminal(AgentTerminal::Generator(
        GeneratorSubmission {
            attempt_id: started.attempt_id.clone(),
            task_id: generator_id,
            status: eos_state::TaskOutcomeStatus::Success,
            outcome: "generated".to_owned(),
            terminal_tool_result: terminal_result(),
        },
    )));
    runner.push(AgentRunReport::terminal(AgentTerminal::Reducer(
        ReducerSubmission {
            attempt_id: started.attempt_id.clone(),
            task_id: reducer_id,
            status: eos_state::TaskOutcomeStatus::Success,
            outcome: "reduced".to_owned(),
            terminal_tool_result: terminal_result(),
        },
    )));
    wait_for_workflow_status(&stores, &started.workflow_id, WorkflowStatus::Succeeded).await;

    assert_eq!(
        stores.attempt(&started.attempt_id).unwrap().status,
        AttemptStatus::Passed
    );
    assert_eq!(
        stores.iteration(&started.iteration_id).unwrap().status,
        IterationStatus::Succeeded
    );
    assert_eq!(
        stores.workflow(&started.workflow_id).unwrap().status,
        WorkflowStatus::Succeeded
    );
    assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
    assert_eq!(runner.launches().len(), 3);
}

#[tokio::test]
async fn no_terminal_generator_fails_the_attempt_and_workflow_when_budget_is_exhausted() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let mut deps = stores.deps(runner.clone());
    deps.lifecycle_config.default_attempt_budget = 1;
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let started = WorkflowStarter::new(deps.clone())
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
    assert_eq!(
        stores.iteration(&started.iteration_id).unwrap().status,
        IterationStatus::Failed
    );
    assert_eq!(
        stores.workflow(&started.workflow_id).unwrap().status,
        WorkflowStatus::Failed
    );
}

#[tokio::test]
async fn launch_failure_marks_task_failed_instead_of_stranding_running_task() {
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
    crate::AttemptStageAdvancer::new(orchestrator)
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

#[tokio::test]
async fn workflow_control_uses_runtime_handles_and_cancels_child_state() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let deps = stores.deps(runner);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let adapter = WorkflowControlAdapter::new(
        WorkflowStarter::new(deps),
        stores.clone(),
        stores.clone(),
        stores.clone(),
        stores.clone(),
    );

    let started = adapter
        .start(&parent.id, "agent-1", "delegated goal")
        .await
        .unwrap();
    assert_eq!(started.workflow_task_id.as_str(), "wf_1");
    let derived_handle: eos_types::WorkflowSessionId =
        format!("wf_{}", started.workflow_id.as_str())
            .parse()
            .unwrap();
    assert!(adapter
        .status(&started.workflow_id, Some(&derived_handle))
        .await
        .unwrap()
        .contains("was not found"));

    adapter
        .cancel(&started.workflow_task_id, "stop now")
        .await
        .unwrap();

    let workflow = stores.workflow(&started.workflow_id).unwrap();
    assert_eq!(workflow.status, WorkflowStatus::Cancelled);
    let iteration_id = workflow.iteration_ids.first().unwrap();
    let iteration = stores.iteration(iteration_id).unwrap();
    assert_eq!(iteration.status, IterationStatus::Cancelled);
    let attempt_id = iteration.attempt_ids.first().unwrap();
    let attempt = stores.attempt(attempt_id).unwrap();
    assert_eq!(attempt.status, AttemptStatus::Failed);
    assert_eq!(attempt.fail_reason, Some(AttemptFailReason::TaskFailed));
    let planner_task = stores
        .task(attempt.planner_task_id.as_ref().unwrap())
        .unwrap();
    assert_eq!(planner_task.status, TaskStatus::Failed);
    assert_eq!(
        planner_task
            .terminal_tool_result
            .unwrap()
            .get("fail_reason"),
        Some(&json!("workflow_cancelled"))
    );
}
