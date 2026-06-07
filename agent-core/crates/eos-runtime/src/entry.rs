//! Request run-to-completion: mint the root task, provision the sandbox, wire the
//! per-request delegated-workflow runtime, run the root agent **inline** through
//! the shared engine primitive, and return the root's terminal outcome.
//!
//! The root is a `Task(role=Root, workflow_id=None)` run directly through the
//! engine — never the workflow starter (GC-eos-runtime-01). Closure is a single
//! framework-side guard (`fail_unfinished_root`); the happy-path writer is the
//! engine-stamped `submit_root_outcome`.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use eos_agent_def::{AgentDefinition, AgentName};
use eos_agent_message_records::AgentRunRecordKind;
use eos_engine::{
    run_agent, AgentRunControlFactory, AgentRunInput, AgentRunRegistry, BackgroundSupervisorFactory,
    ForegroundExecutorFactory,
};
use eos_llm_client::Message;
use eos_state::{RequestStatus, Task, TaskRole, TaskStatus};
use eos_tools::{
    AttemptSubmissionPort, BackgroundSupervisorPort, CommandSessionSupervisorPort,
    WorkflowControlPort,
};
use eos_types::{AgentRunId, JsonObject, RequestId, TaskId};
use eos_workflow::{
    AgentEntryComposer, AgentRunner, AttemptDeps, AttemptOrchestratorRegistry,
    AttemptSubmissionAdapter, ContextEngine, ContextEngineDeps, OpenIterationCoordinatorRegistry,
    WorkflowControlAdapter, WorkflowLifecycleConfig, WorkflowStarter,
};
use serde_json::json;

use crate::agent_runner::RuntimeAgentRunner;
use crate::request_input::RequestRunInput;
use crate::runtime_services::{EventCallback, RuntimeServices};
use crate::tool_context::{build_metadata, MetadataParams};

/// The terminal outcome of a completed top-level request — the root's outcome
/// (canonical flow step 7). The ids are caller-known (`request_id` is injected,
/// `root_task_id` is derived), so they are not echoed here; this struct holds
/// only what the run produces.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    /// Authoritative final status of the root task (`Done` or `Failed`).
    pub status: TaskStatus,
    /// The root's persisted terminal payload (`Task.terminal_tool_result`): the
    /// agent's submitted outcome on success, or `{ fail_reason, summary }`
    /// written by the guard on failure. `Some(_)` on every normal-or-guarded
    /// completion.
    pub terminal: Option<JsonObject>,
}

/// The single source of truth for the root task id derivation (Option A):
/// `root-{request_id}`. Used by `run_request` to mint the row and by tests to
/// know the id before the run completes. The `root-` prefix makes the string
/// statically non-empty, so `TaskId` parsing (which only rejects the empty
/// string) cannot fail.
pub(crate) fn root_task_id_for(request_id: &RequestId) -> TaskId {
    format!("root-{request_id}")
        .parse()
        .expect("root-{request_id} is non-empty, so TaskId parsing cannot fail")
}

/// Run a top-level request to completion and return the root's outcome.
/// `request_id` is minted by the caller (identity is an input, not an output):
/// the root task id is `root-{request_id}`, so both ids are caller-known before
/// the run finishes. Must be called within a Tokio runtime (it spawns the
/// per-request command-completion heartbeat).
///
/// # Errors
/// Returns an error if provisioning, request/root-task creation, or the final
/// root-task read-back fails.
pub async fn run_request(
    services: &RuntimeServices,
    input: RequestRunInput,
    on_event: Option<EventCallback>,
) -> Result<RequestOutcome> {
    let RequestRunInput {
        request_id,
        prompt,
        sandbox_id,
        workspace_root,
        workflow_config,
    } = input;

    // [2] BOOTSTRAP — provision the sandbox and create the request row.
    let binding = services
        .sandbox
        .provisioner
        .prepare_for_run(&request_id, sandbox_id.as_deref())
        .await
        .context("provisioning the request sandbox")?;
    services
        .db
        .request_store
        .create_request(
            &request_id,
            &workspace_root,
            Some(&binding.sandbox_id),
            &prompt,
        )
        .await
        .context("creating the request row")?;

    // Per-agent-run runtime. The request owns only the shared, immutable factory
    // and the live-run registry — never per-agent mutable state. Each
    // root/workflow/subagent run mints one fresh `AgentRunControl` (its own
    // notifier, foreground executor, background supervisor, command-completion
    // heartbeat, and cancellation token); the registry makes live runs
    // addressable for recursive cancellation.
    let control_factory = Arc::new(AgentRunControlFactory::new(
        ForegroundExecutorFactory,
        BackgroundSupervisorFactory::new(
            services.engine_run_handles(&workspace_root),
            services.sandbox.transport.clone(),
            services.engine.command_session_completion_poll_interval(),
        ),
    ));
    let agent_run_registry = AgentRunRegistry::new();
    let iteration_coordinators = Arc::new(OpenIterationCoordinatorRegistry::new());
    let orchestrator_registry = Arc::new(AttemptOrchestratorRegistry::new());
    let context_engine = ContextEngine::new(ContextEngineDeps {
        workflow_store: services.db.workflow_store.clone(),
        iteration_store: services.db.iteration_store.clone(),
        attempt_store: services.db.attempt_store.clone(),
        task_store: services.db.task_store.clone(),
    });
    let composer = Arc::new(AgentEntryComposer::new(
        context_engine,
        services.agent_core.agent_registry.clone(),
    ));
    // The recording attempt-submission port: workflow-agent submit tools record
    // straight to the active per-attempt orchestrator over this shared registry
    // (Path A-recording). Stateless and shared across all runs.
    let attempt_submission: Arc<dyn AttemptSubmissionPort> =
        Arc::new(AttemptSubmissionAdapter::new(orchestrator_registry.clone()));
    // GUARDRAIL: `workflow_control` is built downstream of the runner (starter →
    // attempt_deps → runner), so it is late-bound through this cell and read at
    // run() time — irreducible given the construction cycle.
    let workflow_control_cell: Arc<OnceLock<Arc<dyn WorkflowControlPort>>> =
        Arc::new(OnceLock::new());
    let runner: Arc<dyn AgentRunner> = Arc::new(RuntimeAgentRunner::new(
        services.clone(),
        workspace_root.clone(),
        attempt_submission,
        workflow_control_cell.clone(),
        control_factory.clone(),
        agent_run_registry.clone(),
    ));
    // `attempt_deps` is a local moved into the starter (no clone, never returned).
    let attempt_deps = AttemptDeps::new(
        services.db.workflow_store.clone(),
        services.db.iteration_store.clone(),
        services.db.attempt_store.clone(),
        services.db.task_store.clone(),
        services.agent_core.agent_registry.clone(),
        runner,
    )
    .with_orchestrator_registry(orchestrator_registry)
    .with_iteration_coordinators(iteration_coordinators)
    .with_lifecycle_config(WorkflowLifecycleConfig::default())
    .with_composer(composer)
    .with_max_concurrent_task_runs(workflow_config.attempt.max_concurrent_task_runs);
    let starter = WorkflowStarter::new(attempt_deps);
    let workflow_control: Arc<dyn WorkflowControlPort> = Arc::new(WorkflowControlAdapter::new(
        starter,
        services.db.workflow_store.clone(),
        services.db.iteration_store.clone(),
        services.db.attempt_store.clone(),
        services.db.task_store.clone(),
    ));
    // Late-bind the control port into the workflow-agent runner (closes D1: a
    // nested planner's deferral hook reads workflow_depth; every workflow agent's
    // no-inflight hook reads find_outstanding).
    let _ = workflow_control_cell.set(workflow_control.clone());

    // Root task: `root-{request_id}` (Option A), running, no workflow.
    let root_task_id = root_task_id_for(&request_id);
    services
        .db
        .task_store
        .insert_task(&Task {
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
    services
        .db
        .request_store
        .set_root_task_id(&request_id, &root_task_id)
        .await
        .context("recording the root task id")?;

    // [3][4][5] RUN — resolve the root agent def, build its tool metadata, and run
    // the shared engine primitive INLINE. `submit_root_outcome` closes the task
    // mid-loop on success (step 6, inside the run). `summary` feeds the post-run
    // guard; on the happy path it is unused (the guard no-ops on a closed task).
    let summary = match resolve_root_def(services) {
        Some(root_def) => {
            let agent_run_id = AgentRunId::new_v4();
            // Mint the root's own AgentRunControl and register it as the live run
            // for the root task before the provider loop starts.
            let control = control_factory.persisted(agent_run_id.clone(), root_task_id.clone());
            agent_run_registry.insert(control.clone());
            let metadata = build_metadata(
                &workspace_root,
                MetadataParams {
                    agent_name: "root".to_owned(),
                    sandbox_id: Some(binding.sandbox_id),
                    agent_run_id: agent_run_id.clone(),
                    request_id: Some(request_id.clone()),
                    task_id: Some(root_task_id.clone()),
                    attempt_id: None,
                    workflow_id: None,
                    is_isolated_workspace_mode: false,
                },
            );
            let background = control.background();
            let background_supervisor: Arc<dyn BackgroundSupervisorPort> =
                Arc::new(background.clone());
            let command_session_supervisor: Arc<dyn CommandSessionSupervisorPort> =
                Arc::new(background);
            let run = run_agent(
                &services.engine_run_handles(&workspace_root),
                AgentRunInput {
                    agent: root_def,
                    initial_messages: vec![Message::from_user_text(prompt.clone())],
                    task_id: Some(root_task_id.clone()),
                    agent_run_id,
                    tool_metadata: metadata,
                    attempt_submission: None,
                    workflow_control: Some(workflow_control.clone()),
                    background_supervisor: Some(background_supervisor),
                    command_session_supervisor: Some(command_session_supervisor),
                    notifier: control.notifications(),
                    cancellation: control.cancellation(),
                    foreground: control.foreground(),
                    persist_agent_run: true,
                    record_kind: AgentRunRecordKind::Root,
                },
                on_event.as_ref(),
            )
            .await;
            // The root's own per-run finalizer (inside `run_agent`) already cancelled
            // its subagents/workflows/command sessions. Drop the live-run entry; the
            // control (and its heartbeat) is released at end of scope.
            agent_run_registry.finish_cancel(control.agent_run_id());
            // Success leaves the engine-stamped terminal as the persisted outcome.
            let has_terminal = run.terminal_result.is_some();
            run.error.unwrap_or_else(|| {
                if has_terminal {
                    "root run complete".to_owned()
                } else {
                    "root agent ended without submit_root_outcome".to_owned()
                }
            })
        }
        None => "root agent definition 'root' is not registered".to_owned(),
    };

    // [6] FINALIZE / CLEANUP. Per-agent-run background teardown (heartbeat abort +
    // subagent/workflow/command cancellation) already ran inside each run's own
    // `run_agent` finalizer — there is no request-level supervisor sweep or
    // heartbeat to abort here.
    // The single framework-side closure guard, called unconditionally: a no-op once
    // submit_root_outcome has closed the task; otherwise marks the root Failed +
    // finish_request(Failed).
    fail_unfinished_root(services, &request_id, &root_task_id, &summary).await;
    services.flush_audit();

    // [7] RETURN OUTCOME — single read-back of the persisted root task.
    let task = services
        .db
        .task_store
        .get(&root_task_id)
        .await
        .context("reading the root task outcome")?
        .context("root task row missing after the run")?;
    Ok(RequestOutcome {
        status: task.status,
        terminal: task.terminal_tool_result,
    })
}

/// Resolve the registered `root` agent definition, or `None` when the registry
/// has no `root` profile (the shipped binary seeds one; tests inject one).
fn resolve_root_def(services: &RuntimeServices) -> Option<AgentDefinition> {
    let root_name = AgentName::new("root").ok()?;
    services
        .agent_core
        .agent_registry
        .get(&root_name)
        .map(|def| (**def).clone())
}

/// Fail the root **iff** it is still running (idempotent compare-and-set) and
/// finish the request as `Failed`. Called unconditionally after the inline run; a
/// no-op once `submit_root_outcome` has closed the task. The root role is not in
/// `ExecutionRole`, so the summary rides in `terminal_tool_result` rather than a
/// typed outcome row (documented deviation; the typed outcome column is left
/// empty for root).
async fn fail_unfinished_root(
    services: &RuntimeServices,
    request_id: &RequestId,
    root_task_id: &TaskId,
    summary: &str,
) {
    let mut terminal = JsonObject::new();
    terminal.insert("fail_reason".to_owned(), json!("root_run_exhausted"));
    terminal.insert("summary".to_owned(), json!(summary));

    match services
        .db
        .task_store
        .set_task_status_if_current(
            root_task_id,
            TaskStatus::Running,
            TaskStatus::Failed,
            None,
            Some(&terminal),
        )
        .await
    {
        Ok(Some(_)) => {
            if let Err(err) = services
                .db
                .request_store
                .finish_request(request_id, RequestStatus::Failed)
                .await
            {
                tracing::warn!(error = %err, "finish_request(failed) failed for unfinished root");
            }
        }
        // Task is no longer running (a real terminal won) — do not clobber it.
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(error = %err, "unfinished-root guard could not read the root task");
        }
    }
}
