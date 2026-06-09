//! Agent-runner doubles (`QueueRunner`, `ScriptedRunner`), the recording-port
//! wiring, the workflow agent registry, and small plan/task builders shared by
//! the per-module AC tests.
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_types::{
    AgentDefinition, AgentName, AgentRegistry, AgentRegistryBuilder, AgentType, DeferredGoal,
    GeneratorSubmission, JsonObject, PlanDisposition, PlanNodeId, ReducerSubmission, RequestId,
    Task, TaskOutcomeStatus, TaskRole, TaskStatus, WorkflowId, WorkflowStatus,
};
use eos_types::{AttemptSubmissionPort, PlanReducer, PlanTask, PlannerPlan};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;

use crate::attempt::{AgentLaunch, AgentRunReport, AgentRunner, AttemptOrchestratorRegistry};
use crate::{AttemptSubmissionAdapter, Result};

use super::stores::MemoryStores;

fn node(id: &str) -> PlanNodeId {
    PlanNodeId::new(id).unwrap()
}

/// A scripted terminal submission a test double records via the recording
/// [`AttemptSubmissionPort`] during `run()` — the same tool->record path the real
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
/// production `AttemptSubmissionAdapter` wiring at the composition root).
fn recording_port(registry: &Arc<AttemptOrchestratorRegistry>) -> Arc<dyn AttemptSubmissionPort> {
    Arc::new(AttemptSubmissionAdapter::new(registry.clone()))
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
    port: OnceLock<Arc<dyn AttemptSubmissionPort>>,
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
        record_scripted(&self.port, submission).await
    }
}

/// Record a scripted submission via the recording port (the same path the real
/// submit tools take). A `NoSubmission` records nothing (and needs no bound
/// port), so the owning loop's still-RUNNING guard synthesizes `run_exhausted`;
/// a recording variant resolves the bound port and fails loud if a test forgot
/// to `bind()` after `deps()`.
async fn record_scripted(
    port: &OnceLock<Arc<dyn AttemptSubmissionPort>>,
    submission: ScriptedSubmission,
) -> Result<AgentRunReport> {
    fn bound(port: &OnceLock<Arc<dyn AttemptSubmissionPort>>) -> &Arc<dyn AttemptSubmissionPort> {
        port.get()
            .expect("recording port bound (call bind() after deps())")
    }
    match submission {
        ScriptedSubmission::NoSubmission(summary) => Ok(AgentRunReport::failed(summary)),
        ScriptedSubmission::Planner(plan) => {
            bound(port)
                .apply_plan(plan)
                .await
                .expect("record plan via port");
            Ok(AgentRunReport::ok())
        }
        ScriptedSubmission::Generator(submission) => {
            bound(port)
                .submit_generator(submission)
                .await
                .expect("record generator via port");
            Ok(AgentRunReport::ok())
        }
        ScriptedSubmission::Reducer(submission) => {
            bound(port)
                .apply_reducer(submission)
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
    port: OnceLock<Arc<dyn AttemptSubmissionPort>>,
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
                id: node(&format!("g{i}")),
                agent_name: "coder".to_owned(),
                needs: Vec::new(),
            })
            .collect();
        let task_specs = (0..self.generators)
            .map(|i| (node(&format!("g{i}")), format!("do work {i}")))
            .collect();
        let reducer_needs = (0..self.generators)
            .map(|i| node(&format!("g{i}")))
            .collect();
        PlannerPlan {
            attempt_id: launch.attempt_id().clone(),
            planner_task_id: launch.task_id().clone(),
            disposition: if defer {
                PlanDisposition::Defer(DeferredGoal::new(self.deferred_goal.clone()).unwrap())
            } else {
                PlanDisposition::Complete
            },
            tasks,
            task_specs,
            reducers: vec![PlanReducer {
                id: node("r1"),
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
        let attempt_id = launch.attempt_id().clone();
        let submission = match launch.role() {
            TaskRole::Planner => ScriptedSubmission::Planner(self.build_plan(&launch)),
            TaskRole::Generator => {
                self.enter();
                for _ in 0..4 {
                    tokio::task::yield_now().await;
                }
                self.exit();
                ScriptedSubmission::Generator(GeneratorSubmission {
                    attempt_id,
                    task_id: launch.task_id().clone(),
                    status: TaskOutcomeStatus::Success,
                    outcome: "generated".to_owned(),
                    terminal_tool_result: terminal_tool_result_fixture(),
                })
            }
            TaskRole::Reducer => {
                self.enter();
                tokio::task::yield_now().await;
                self.exit();
                ScriptedSubmission::Reducer(ReducerSubmission {
                    attempt_id,
                    task_id: launch.task_id().clone(),
                    status: self.reducer_status,
                    outcome: "reduced".to_owned(),
                    terminal_tool_result: terminal_tool_result_fixture(),
                })
            }
            other => panic!("ScriptedRunner does not serve role {other:?}"),
        };
        record_scripted(&self.port, submission).await
    }
}

pub(crate) fn agent_registry() -> AgentRegistry {
    let mut builder = AgentRegistryBuilder::new();
    for (name, role, terminals) in [
        ("root", TaskRole::Root, vec!["submit_root_outcome"]),
        ("planner", TaskRole::Planner, vec!["submit_plan"]),
        (
            "coder",
            TaskRole::Generator,
            vec!["submit_generator_outcome"],
        ),
        ("reducer", TaskRole::Reducer, vec!["submit_reducer_outcome"]),
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
        ("root", TaskRole::Root, vec!["submit_root_outcome"]),
        (
            "coder",
            TaskRole::Generator,
            vec!["submit_generator_outcome"],
        ),
        ("reducer", TaskRole::Reducer, vec!["submit_reducer_outcome"]),
    ] {
        builder.add(agent_def(name, role, terminals));
    }
    builder.build()
}

fn agent_def(name: &str, role: TaskRole, terminals: Vec<&str>) -> AgentDefinition {
    AgentDefinition {
        name: AgentName::new(name).unwrap(),
        description: format!("{name} agent"),
        system_prompt: None,
        model: None,
        tool_call_limit: NonZeroU32::new(16).unwrap(),
        agent_type: AgentType::Agent,
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

pub(crate) fn terminal_tool_result_fixture() -> JsonObject {
    json!({"ok": true}).as_object().unwrap().clone()
}

/// A full one-generator/one-reducer plan keyed to a started workflow's attempt.
pub(crate) fn one_step_plan(started: &crate::StartedWorkflow) -> PlannerPlan {
    PlannerPlan {
        attempt_id: started.attempt_id.clone(),
        planner_task_id: crate::planner_task_id(&started.attempt_id).unwrap(),
        disposition: PlanDisposition::Complete,
        tasks: vec![PlanTask {
            id: node("g1"),
            agent_name: "coder".to_owned(),
            needs: Vec::new(),
        }],
        task_specs: [(node("g1"), "do work".to_owned())].into_iter().collect(),
        reducers: vec![PlanReducer {
            id: node("r1"),
            needs: vec![node("g1")],
            prompt: "reduce".to_owned(),
        }],
    }
}

/// Spin the test runtime until `predicate` holds, or panic. The single waiter
/// (`TESTING_SPEC` §4.4 / AC3): every mid-flight checkpoint predicate — a launched
/// role, an attempt stage, a workflow status — funnels through here, so there is
/// no parallel waiter.
pub(crate) async fn wait_until<F: FnMut() -> bool>(mut predicate: F) {
    for _ in 0..5000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("wait_until: predicate not satisfied within the spin budget");
}

/// Spin until `workflow_id` reaches `status` — a thin [`wait_until`] wrapper.
pub(crate) async fn wait_for_workflow_status(
    stores: &MemoryStores,
    workflow_id: &WorkflowId,
    status: WorkflowStatus,
) {
    wait_until(|| stores.workflow(workflow_id).map(|w| w.status) == Some(status)).await;
}
