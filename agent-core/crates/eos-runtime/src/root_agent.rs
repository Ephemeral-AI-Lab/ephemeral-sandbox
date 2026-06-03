//! The root-agent lifecycle: run the `root` agent directly through the engine
//! loop (no root workflow) and apply the `_fail_unfinished_root` guard.
//!
//! Ports `runtime/entry.py::_run_root_agent` / `_fail_unfinished_root`.

use std::sync::Arc;

use eos_agent_def::AgentName;
use eos_llm_client::Message;
use eos_state::TaskStatus;
use eos_tools::{SubagentSupervisorPort, WorkflowControlPort};
use eos_types::{AgentRunId, JsonObject, RequestId, SandboxId, TaskId};
use serde_json::json;

use crate::agent_loop::{run_ephemeral_agent, EphemeralRunInput};
use crate::app_state::{AppState, EventCallback};
use crate::tool_context::{build_metadata, MetadataParams};

/// Everything one root-agent run needs beyond the shared [`AppState`].
pub(crate) struct RootAgentParams {
    pub request_id: RequestId,
    pub root_task_id: TaskId,
    pub prompt: String,
    pub sandbox_id: SandboxId,
    pub workflow_control: Arc<dyn WorkflowControlPort>,
    pub subagent_supervisor: Arc<dyn SubagentSupervisorPort>,
    pub on_event: Option<EventCallback>,
}

/// Run the root agent to completion, then apply the unfinished-root guard.
pub(crate) async fn run_root_agent(state: AppState, params: RootAgentParams) {
    let Ok(root_name) = AgentName::new("root") else {
        fail_unfinished_root(
            &state,
            &params.request_id,
            &params.root_task_id,
            "root agent name is invalid",
        )
        .await;
        return;
    };
    let Some(root_def) = state
        .agent_registry
        .get(&root_name)
        .map(|def| (**def).clone())
    else {
        fail_unfinished_root(
            &state,
            &params.request_id,
            &params.root_task_id,
            "root agent definition 'root' is not registered",
        )
        .await;
        return;
    };

    let agent_run_id = AgentRunId::new_v4();
    let metadata = build_metadata(
        &state,
        MetadataParams {
            agent_name: "root".to_owned(),
            sandbox_id: Some(params.sandbox_id.clone()),
            agent_run_id: agent_run_id.clone(),
            request_id: Some(params.request_id.clone()),
            task_id: Some(params.root_task_id.clone()),
            attempt_id: None,
            workflow_id: None,
            workflow_control: Some(params.workflow_control.clone()),
            subagent_supervisor: Some(params.subagent_supervisor.clone()),
        },
    );

    let run = run_ephemeral_agent(
        &state,
        EphemeralRunInput {
            agent: root_def,
            initial_messages: vec![Message::from_user_text(params.prompt.clone())],
            task_id: Some(params.root_task_id.clone()),
            agent_run_id,
            tool_metadata: metadata,
            persist_agent_run: true,
        },
        params.on_event.as_ref(),
    )
    .await;

    // Success leaves the engine-stamped terminal as the persisted outcome.
    if run.error.is_some() || run.terminal_result.is_none() {
        let summary = run
            .error
            .unwrap_or_else(|| "root agent ended without submit_root_outcome".to_owned());
        fail_unfinished_root(&state, &params.request_id, &params.root_task_id, &summary).await;
    }
}

/// Mark a still-running root task failed and finish the request. Runs only when
/// the task is **still `running`** (the engine may have stamped a real terminal),
/// using a compare-and-set so the guard is atomic. The root role is not in
/// `ExecutionRole`, so the summary rides in `terminal_tool_result` rather than a
/// typed outcome row (documented deviation; the typed outcome column is left
/// empty for root).
pub(crate) async fn fail_unfinished_root(
    state: &AppState,
    request_id: &RequestId,
    root_task_id: &TaskId,
    summary: &str,
) {
    let mut terminal = JsonObject::new();
    terminal.insert("fail_reason".to_owned(), json!("root_run_exhausted"));
    terminal.insert("summary".to_owned(), json!(summary));

    match state
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
            if let Err(err) = state
                .request_store
                .finish_request(request_id, "failed")
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
