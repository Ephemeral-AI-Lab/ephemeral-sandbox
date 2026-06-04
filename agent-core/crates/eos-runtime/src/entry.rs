//! Request bootstrap: mint the root task, provision the sandbox, wire the
//! per-request delegated-workflow runtime, and spawn the root agent.
//!
//! Ports `runtime/entry.py::RequestEntry` / `start_request` / `_create_runtime`.
//! The root is a `Task(role=root, workflow_id=None)` run directly through the
//! engine — never the workflow starter (GC-eos-runtime-01).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use eos_engine::{
    spawn_command_completion_heartbeat, BackgroundSupervisorHandle, NotificationService,
};
use eos_state::{Task, TaskRole, TaskStatus};
use eos_tools::{
    BackgroundSupervisorPort, CommandSessionSupervisorPort, NotificationSink, PlanSubmissionPort,
    WorkflowControlPort,
};
use eos_types::{RequestId, TaskId};
use eos_workflow::{
    AgentEntryComposer, AgentRunner, AttemptDeps, AttemptOrchestratorRegistry, ContextEngine,
    ContextEngineDeps, OpenIterationCoordinatorRegistry, PlanSubmissionAdapter,
    WorkflowControlAdapter, WorkflowLifecycleConfig, WorkflowStarter,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::agent_runner::RuntimeAgentRunner;
use crate::app_state::{AppState, EventCallback};
use crate::root_agent::{fail_unfinished_root, run_root_agent, RootAgentParams};

/// Handle to a started request: the minted ids, the per-request workflow
/// dependency bundle, and the spawned root-agent task.
#[non_exhaustive]
pub struct RequestEntryHandle {
    /// The top-level request id.
    pub request_id: RequestId,
    /// The root task id (`root-<hex16>`).
    pub root_task_id: TaskId,
    /// The runtime-wired per-request delegated-workflow dependency bundle.
    pub attempt_deps: AttemptDeps,
    pub(crate) root_agent_task: JoinHandle<()>,
    pub(crate) supervisor: Arc<BackgroundSupervisorHandle>,
    pub(crate) workflow_control: Arc<dyn WorkflowControlPort>,
    /// Per-request command-completion heartbeat (anchor §5.3); aborted at
    /// request teardown.
    pub(crate) heartbeat: JoinHandle<()>,
    pub(crate) state: AppState,
    finished: bool,
}

impl std::fmt::Debug for RequestEntryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestEntryHandle")
            .field("request_id", &self.request_id)
            .field("root_task_id", &self.root_task_id)
            .finish_non_exhaustive()
    }
}

impl Drop for RequestEntryHandle {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.state.shutdown.cancel();
        self.heartbeat.abort();
        self.root_agent_task.abort();

        let supervisor = self.supervisor.clone();
        let workflow_control = self.workflow_control.clone();
        let state = self.state.clone();
        let request_id = self.request_id.clone();
        let root_task_id = self.root_task_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                request_id = self.request_id.as_str(),
                "request handle dropped outside a Tokio runtime; background cleanup could not be spawned"
            );
            return;
        };
        handle.spawn(async move {
            supervisor
                .cancel_for_parent_exit(
                    "",
                    Some(workflow_control),
                    "request handle dropped before join/shutdown",
                )
                .await;
            fail_unfinished_root(
                &state,
                &request_id,
                &root_task_id,
                "request handle dropped before join/shutdown",
            )
            .await;
            state.flush_audit();
        });
    }
}

impl RequestEntryHandle {
    /// Await the root agent to completion. On a join error (the spawned task
    /// panicked or was aborted), apply the still-running unfinished-root guard so
    /// a crash persists a failure instead of leaving the task running
    /// (AC-eos-runtime-03b).
    pub async fn join(mut self) {
        let root_agent_task = std::mem::replace(&mut self.root_agent_task, tokio::spawn(async {}));
        let result = root_agent_task.await;
        // The root run is done; the heartbeat has nothing left to deliver.
        self.heartbeat.abort();
        self.supervisor
            .cancel_for_parent_exit(
                "",
                Some(self.workflow_control.clone()),
                "request root task joined",
            )
            .await;
        if let Err(join_err) = result {
            let summary = format!("root agent task did not complete: {join_err}");
            fail_unfinished_root(&self.state, &self.request_id, &self.root_task_id, &summary).await;
        }
        self.finished = true;
    }

    /// Graceful shutdown: cancel the token, parent-exit the background
    /// supervisor, await the root task within `grace`, abort on timeout, run the
    /// unfinished-root guard, then flush audit (AC-eos-runtime-08b).
    pub async fn shutdown(mut self, reason: &str, grace: Duration) {
        self.state.shutdown.cancel();
        self.heartbeat.abort();
        self.supervisor
            .cancel_for_parent_exit("", Some(self.workflow_control.clone()), reason)
            .await;

        let root_agent_task = std::mem::replace(&mut self.root_agent_task, tokio::spawn(async {}));
        let abort = root_agent_task.abort_handle();
        if tokio::time::timeout(grace, root_agent_task).await.is_err() {
            abort.abort();
        }
        let summary = format!("request shutdown: {reason}");
        fail_unfinished_root(&self.state, &self.request_id, &self.root_task_id, &summary).await;
        self.state.flush_audit();
        self.finished = true;
    }
}

/// Start a top-level request: provision the sandbox, create the request + root
/// task, wire the delegated-workflow runtime, and spawn the root agent. Must be
/// called within a Tokio runtime (it spawns the root-agent task).
///
/// # Errors
/// Returns an error if provisioning, request/root-task creation, or root-task-id
/// minting fails.
pub async fn start_request(
    state: &AppState,
    prompt: impl Into<String>,
    sandbox_id: Option<&str>,
    on_event: Option<EventCallback>,
) -> Result<RequestEntryHandle> {
    let prompt = prompt.into();

    let request_id = RequestId::new_v4();
    let binding = state
        .provisioner
        .prepare_for_run(&request_id, sandbox_id)
        .await
        .context("provisioning the request sandbox")?;
    state
        .request_store
        .create_request(&request_id, &state.cwd, Some(&binding.sandbox_id), &prompt)
        .await
        .context("creating the request row")?;

    // Per-request delegated-workflow runtime (Python `_create_runtime`). The
    // single supervisor carries the engine run handles the subagent driver needs
    // (it calls `run_ephemeral_agent` directly).
    let supervisor = Arc::new(BackgroundSupervisorHandle::new(
        state.engine_run_handles(),
        state.transport.clone(),
    ));
    let background_supervisor_port: Arc<dyn BackgroundSupervisorPort> = supervisor.clone();
    // One NotificationService per request: its queue is shared by the tool sink,
    // the heartbeat, and (via the loop's `notifier`) the query loop — the §7
    // instance-identity invariant. The command-session port is the SAME
    // supervisor instance the heartbeat pulls into.
    let notifier = NotificationService::new();
    let notification_sink: Arc<dyn NotificationSink> = Arc::new(notifier.clone());
    let command_session_port: Arc<dyn CommandSessionSupervisorPort> = supervisor.clone();
    let heartbeat = spawn_command_completion_heartbeat(
        supervisor.inner(),
        notification_sink,
        state.transport.clone(),
    );
    let iteration_coordinators = Arc::new(OpenIterationCoordinatorRegistry::new());
    let orchestrator_registry = Arc::new(AttemptOrchestratorRegistry::new());
    let context_engine = ContextEngine::new(ContextEngineDeps {
        workflow_store: state.workflow_store.clone(),
        iteration_store: state.iteration_store.clone(),
        attempt_store: state.attempt_store.clone(),
        task_store: state.task_store.clone(),
    });
    let composer = Arc::new(AgentEntryComposer::new(
        context_engine,
        state.agent_registry.clone(),
    ));
    // The recording plan-submission port: workflow-agent submit tools record
    // straight to the active per-attempt orchestrator over this shared registry
    // (Path A-recording). Stateless and shared across all runs.
    let plan_submission: Arc<dyn PlanSubmissionPort> =
        Arc::new(PlanSubmissionAdapter::new(orchestrator_registry.clone()));
    // `workflow_control` is built downstream of the runner (starter → attempt_deps
    // → runner), so it is late-bound through this cell and read at run() time.
    let workflow_control_cell: Arc<OnceLock<Arc<dyn WorkflowControlPort>>> =
        Arc::new(OnceLock::new());
    let runner: Arc<dyn AgentRunner> = Arc::new(RuntimeAgentRunner::new(
        state.clone(),
        plan_submission,
        workflow_control_cell.clone(),
        background_supervisor_port.clone(),
        command_session_port.clone(),
        notifier.clone(),
    ));
    let attempt_deps = AttemptDeps {
        workflow_store: state.workflow_store.clone(),
        iteration_store: state.iteration_store.clone(),
        attempt_store: state.attempt_store.clone(),
        task_store: state.task_store.clone(),
        agent_registry: state.agent_registry.clone(),
        orchestrator_registry,
        iteration_coordinators: Some(iteration_coordinators),
        lifecycle_config: WorkflowLifecycleConfig::default(),
        composer: Some(composer),
        runner,
        max_concurrent_task_runs: state.config.attempt.max_concurrent_task_runs,
    };
    let starter = WorkflowStarter::new(attempt_deps.clone());
    let workflow_control: Arc<dyn WorkflowControlPort> = Arc::new(WorkflowControlAdapter::new(
        starter,
        state.workflow_store.clone(),
        state.iteration_store.clone(),
        state.attempt_store.clone(),
        state.task_store.clone(),
    ));
    // Late-bind the control port into the workflow-agent runner (closes D1: a
    // nested planner's deferral hook reads workflow_depth; every workflow agent's
    // no-inflight hook reads find_outstanding).
    let _ = workflow_control_cell.set(workflow_control.clone());

    // Root task: `root-<hex16>`, running, no workflow (non-goal: no root workflow).
    let root_task_id: TaskId = format!("root-{}", &Uuid::new_v4().simple().to_string()[..16])
        .parse()
        .context("minting root task id")?;
    state
        .task_store
        .upsert_task(&Task {
            id: root_task_id.clone(),
            request_id: request_id.clone(),
            role: TaskRole::Root,
            instruction: prompt.clone(),
            status: TaskStatus::Running,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: Some("root".to_owned()),
            needs: Vec::new(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        })
        .await
        .context("creating the root task")?;
    state
        .request_store
        .set_root_task_id(&request_id, &root_task_id)
        .await
        .context("recording the root task id")?;

    let workflow_control_handle = workflow_control.clone();
    let params = RootAgentParams {
        request_id: request_id.clone(),
        root_task_id: root_task_id.clone(),
        prompt,
        sandbox_id: binding.sandbox_id,
        workflow_control,
        background_supervisor: background_supervisor_port,
        command_session_supervisor: command_session_port,
        notifier,
        on_event,
    };
    let root_state = state.clone();
    let root_agent_task = tokio::spawn(async move {
        run_root_agent(root_state, params).await;
    });

    Ok(RequestEntryHandle {
        request_id,
        root_task_id,
        attempt_deps,
        root_agent_task,
        supervisor,
        workflow_control: workflow_control_handle,
        heartbeat,
        state: state.clone(),
        finished: false,
    })
}
