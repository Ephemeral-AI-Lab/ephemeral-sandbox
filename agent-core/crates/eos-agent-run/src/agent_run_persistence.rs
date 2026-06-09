//! Agent-run persistence helpers owned by the runner.

use eos_tool::ToolResult;
use eos_types::{
    AgentRun, AgentRunId, AgentRunOutcome, AgentRunStatus, AgentRunStore, JsonObject, TaskId,
};
use serde_json::json;

use crate::AgentRunError;

pub(crate) async fn create_agent_run_if_requested(
    store: &dyn AgentRunStore,
    persist_agent_run: bool,
    task_id: Option<&TaskId>,
    agent_run_id: &AgentRunId,
    agent_name: &str,
) -> Result<bool, AgentRunError> {
    if !persist_agent_run {
        return Ok(false);
    }
    store
        .create_run(agent_run_id, task_id, agent_name, None)
        .await
        .map_err(|err| AgentRunError::Internal(err.to_string()))?;
    Ok(true)
}

pub(crate) async fn finish_agent_run_if_requested(
    store: &dyn AgentRunStore,
    persistence_requested: bool,
    agent_run_id: &AgentRunId,
    submission_outcome: Option<&ToolResult>,
    token_count: Option<i64>,
    error: Option<&str>,
) -> Result<(), AgentRunError> {
    if !persistence_requested {
        return Ok(());
    }
    let submission_payload = submission_outcome.map(tool_result_payload);
    store
        .finish_run(
            agent_run_id,
            None,
            submission_payload.as_ref(),
            token_count.unwrap_or_default(),
            error,
        )
        .await
        .map(|_| ())
        .map_err(|err| AgentRunError::Internal(err.to_string()))
}

pub(crate) async fn finish_agent_run_cancelled(
    store: &dyn AgentRunStore,
    agent_run_id: &AgentRunId,
    reason: &str,
) -> Result<(), AgentRunError> {
    let payload = cancelled_payload(reason);
    store
        .finish_run(agent_run_id, None, Some(&payload), 0, Some(reason))
        .await
        .map(|_| ())
        .map_err(|err| AgentRunError::Internal(err.to_string()))
}

pub(crate) fn completion_from_agent_run(
    agent_run_id: &AgentRunId,
    run: &AgentRun,
) -> Option<AgentRunOutcome> {
    run.finished_at?;
    if let Some(terminal) = &run.terminal_tool_result {
        if is_cancelled_payload(terminal) {
            return Some(AgentRunOutcome {
                agent_run_id: agent_run_id.clone(),
                status: AgentRunStatus::Cancelled,
                submission_payload: None,
                message_history: Vec::new(),
                token_count: Some(run.token_count),
                error: run.error.clone(),
            });
        }
        return Some(AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Completed,
            submission_payload: Some(terminal.clone()),
            message_history: Vec::new(),
            token_count: Some(run.token_count),
            error: run.error.clone(),
        });
    }
    let message = match &run.error {
        Some(error) => format!("agent run failed: {error}"),
        None => "agent run failed without terminal outcome".to_owned(),
    };
    Some(AgentRunOutcome {
        agent_run_id: agent_run_id.clone(),
        status: AgentRunStatus::Failed,
        submission_payload: None,
        message_history: Vec::new(),
        token_count: Some(run.token_count),
        error: Some(message),
    })
}

pub(crate) fn tool_result_payload(result: &ToolResult) -> JsonObject {
    let mut payload = JsonObject::new();
    payload.insert("output".to_owned(), json!(result.output));
    payload.insert("is_error".to_owned(), json!(result.is_error));
    payload.insert("metadata".to_owned(), json!(result.metadata));
    payload.insert("is_terminal".to_owned(), json!(result.is_terminal));
    payload
}

fn cancelled_payload(reason: &str) -> JsonObject {
    let mut payload = JsonObject::new();
    payload.insert("fail_reason".to_owned(), json!("cancelled"));
    payload.insert("reason".to_owned(), json!(reason));
    payload
}

fn is_cancelled_payload(payload: &JsonObject) -> bool {
    payload
        .get("fail_reason")
        .and_then(serde_json::Value::as_str)
        == Some("cancelled")
}
