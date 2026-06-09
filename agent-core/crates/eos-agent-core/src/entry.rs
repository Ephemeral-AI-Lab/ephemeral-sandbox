//! Request run-to-completion: mint the root task, provision the sandbox, wire the
//! per-request delegated-workflow runtime, start the root agent through
//! `AgentRunApi`, and return the root's terminal outcome.
//!
//! The root is a `Task(role=Root, workflow_id=None)` started through the
//! agent-run lifecycle — never the workflow starter (GC-eos-agent-core-01). Closure is a single
//! framework-side guard (`fail_unfinished_root`); the happy-path writer is the
//! engine-stamped `submit_root_outcome`.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use eos_agent_run::AgentRunService as RunnerAgentRunService;
use eos_llm_client::Message;
use eos_types::{
    AgentCoreCancellationApi, AgentName, AgentName as SpawnAgentName, AgentRunApi, AgentRunId,
    JsonObject, RequestId, SpawnAgentRequest, SpawnAgentTarget, TaskId, WorkflowApi,
    WorkflowAttemptSubmissionApi,
};
use eos_types::{RequestStatus, Task, TaskRole, TaskStatus};
use eos_workflow::{
    AgentEntryComposer, AgentRunner, AttemptOrchestratorRegistry, AttemptResources,
    AttemptSubmissionAdapter, ContextEngine, ContextEngineStores, OpenIterationCoordinatorRegistry,
    WorkflowLifecycleConfig, WorkflowService, WorkflowStarter,
};
use serde_json::json;

use crate::agent_runner::RuntimeAgentRunner;
use crate::request_input::RequestRunInput;
use crate::runtime::{
    build_agent_loop_launcher, AgentCoreRuntime, EngineEventSink, RuntimeAgentCoreCancellation,
};

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
    services: &AgentCoreRuntime,
    input: RequestRunInput,
    on_event: Option<EngineEventSink>,
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

    // GUARDRAIL: `WorkflowService` owns the starter, and the starter captures
    // this runner before the workflow service itself exists. The cell is set
    // before any workflow task agent can be launched.
    let workflow_service_cell: Arc<OnceLock<Arc<dyn WorkflowApi>>> = Arc::new(OnceLock::new());
    let iteration_coordinators = Arc::new(OpenIterationCoordinatorRegistry::new());
    let orchestrator_registry = Arc::new(AttemptOrchestratorRegistry::new());
    let context_engine = ContextEngine::new(ContextEngineStores {
        workflow_store: services.db.workflow_store.clone(),
        iteration_store: services.db.iteration_store.clone(),
        attempt_store: services.db.attempt_store.clone(),
        task_store: services.db.task_store.clone(),
    });
    let composer = Arc::new(AgentEntryComposer::new(
        context_engine,
        services.agent_core.agent_registry.clone(),
    ));
    // The recording attempt-submission API: workflow-agent submit tools record
    // straight to the active per-attempt orchestrator over this shared registry
    // (Path A-recording). Stateless and shared across all runs.
    let attempt_submission: Arc<dyn WorkflowAttemptSubmissionApi> =
        Arc::new(AttemptSubmissionAdapter::new(orchestrator_registry.clone()));
    // `WorkflowService` needs cancellation before the root `AgentRunService`
    // exists; the API observes this request-scoped slot only for cancellation.
    let agent_run_api_cell: Arc<OnceLock<Arc<dyn AgentRunApi>>> = Arc::new(OnceLock::new());
    let runner: Arc<dyn AgentRunner> = Arc::new(RuntimeAgentRunner::new(
        services.clone(),
        workspace_root.clone(),
        attempt_submission.clone(),
        workflow_service_cell.clone(),
        on_event.clone(),
    ));
    // `attempt_deps` is a local moved into the starter (no clone, never returned).
    let attempt_deps = AttemptResources::new(
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
    let cancellation_api: Arc<dyn AgentCoreCancellationApi> =
        Arc::new(RuntimeAgentCoreCancellation::new(
            services.db.task_store.clone(),
            services.db.agent_run_store.clone(),
            agent_run_api_cell.clone(),
        ));
    // Publish this request's cancellation API so `cancel_agent_core_user_request`
    // (called from another task) can reach it. The guard removes it when
    // `run_request` returns or unwinds, so the API (and the registry/stores it
    // holds) cannot leak.
    let _cancel_guard = services
        .cancel_registry
        .register(request_id.clone(), cancellation_api.clone());
    let workflow_service: Arc<dyn WorkflowApi> = Arc::new(WorkflowService::new(
        starter,
        services.db.workflow_store.clone(),
        services.db.iteration_store.clone(),
        services.db.attempt_store.clone(),
        services.db.task_store.clone(),
        cancellation_api,
    ));
    // Publish the completed workflow service to the already-captured workflow
    // runner before root or delegated agents start.
    let _ = workflow_service_cell.set(workflow_service.clone());
    let loop_launcher = build_agent_loop_launcher(
        services,
        attempt_submission,
        workflow_service.clone(),
        on_event,
    );

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

    // [3][4][5] RUN — task-trigger the root through the runner-owned
    // AgentRunApi. The runner owns the durable row lifecycle and receives loop
    // completion through the `AgentLoopLauncher` outcome channel.
    let summary = match AgentName::new("root") {
        Ok(root_name) => {
            let agent_runs = Arc::new(
                RunnerAgentRunService::new(
                    services.agent_core.agent_registry.clone(),
                    loop_launcher,
                    services.db.agent_run_store.clone(),
                    services.message_records.message_records.clone(),
                )
                .with_runtime_state_hooks(
                    {
                        let agent_state = services.agent_state.clone();
                        move |request, agent_run_id| {
                            agent_state.record_spawn_request(request, agent_run_id)
                        }
                    },
                    {
                        let agent_state = services.agent_state.clone();
                        move |agent_run_id| agent_state.remove(agent_run_id)
                    },
                ),
            );
            let agent_run_api: Arc<dyn AgentRunApi> = agent_runs.clone();
            let _ = agent_run_api_cell.set(agent_run_api);
            match agent_runs
                .spawn_agent(SpawnAgentRequest {
                    agent_name: SpawnAgentName::new(root_name.as_str())
                        .expect("root agent name is valid"),
                    agent_run_id: Some(AgentRunId::new_v4()),
                    initial_messages: vec![Message::from_user_text(prompt.clone())],
                    target: SpawnAgentTarget::Root {
                        request_id: request_id.clone(),
                        task_id: root_task_id.clone(),
                    },
                    sandbox_id: Some(binding.sandbox_id.clone()),
                    workspace_root: workspace_root.clone(),
                    is_isolated_workspace_mode: false,
                    persist: true,
                })
                .await
            {
                Ok(agent_run_id) => match agent_runs.wait_for_agent_outcome(&agent_run_id).await {
                    Ok(outcome) => {
                        let has_submission = outcome.submission_payload.is_some();
                        outcome.error.unwrap_or_else(|| {
                            if has_submission {
                                "root run complete".to_owned()
                            } else {
                                "root agent ended without submit_root_outcome".to_owned()
                            }
                        })
                    }
                    Err(err) => err.to_string(),
                },
                Err(err) => err.to_string(),
            }
        }
        Err(err) => format!("invalid root agent name: {err}"),
    };

    // [6] FINALIZE / CLEANUP. Agent-run row finalization has already happened in
    // the runner. The request-level root guard below remains the idempotent
    // protection against a root that exits without submitting.
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

/// Fail the root **iff** it is still running (idempotent compare-and-set) and
/// finish the request as `Failed`. Called unconditionally after the root agent
/// has completed or failed; a no-op once `submit_root_outcome` has closed the
/// task. The root role is not in `ExecutionRole`, so the summary rides in
/// `terminal_tool_result` rather than a typed outcome row (documented deviation;
/// the typed outcome column is left empty for root).
async fn fail_unfinished_root(
    services: &AgentCoreRuntime,
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
