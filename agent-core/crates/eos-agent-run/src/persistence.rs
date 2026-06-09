//! Agent-run persistence helpers owned by the runner.

use eos_types::{
    AgentRun, AgentRunId, AgentRunOutcome, AgentRunStatus, AgentRunStore, JsonObject,
    ParentedOutcome, TaskAgentRunStore, TaskId, TaskOutcome, TaskStatus,
};
use serde_json::json;

use crate::AgentRunError;

pub(crate) async fn create_agent_run(
    store: &dyn AgentRunStore,
    task_id: Option<&TaskId>,
    agent_run_id: &AgentRunId,
    agent_name: &str,
) -> Result<(), AgentRunError> {
    store
        .create_run(agent_run_id, task_id, agent_name)
        .await
        .map_err(|err| AgentRunError::Internal(err.to_string()))?;
    Ok(())
}

pub(crate) async fn finish_agent_run(
    store: &dyn AgentRunStore,
    agent_run_id: &AgentRunId,
    submission_payload: Option<&JsonObject>,
    token_count: Option<i64>,
    error: Option<&str>,
) -> Result<(), AgentRunError> {
    store
        .finish_run(
            agent_run_id,
            submission_payload,
            token_count.unwrap_or_default(),
            error,
        )
        .await
        .map(|_| ())
        .map_err(|err| AgentRunError::Internal(err.to_string()))
}

pub(crate) async fn finish_cancelled_agent_run(
    store: &dyn AgentRunStore,
    agent_run_id: &AgentRunId,
    reason: &str,
) -> Result<(), AgentRunError> {
    let payload = cancelled_payload(reason);
    store
        .finish_run(agent_run_id, Some(&payload), 0, Some(reason))
        .await
        .map(|_| ())
        .map_err(|err| AgentRunError::Internal(err.to_string()))
}

pub(crate) async fn finish_task_agent_run(
    store: &dyn TaskAgentRunStore,
    agent_run_id: &AgentRunId,
    status: TaskStatus,
    terminal_payload: Option<&JsonObject>,
    token_count: i64,
    error: Option<&str>,
) -> Result<(), AgentRunError> {
    let task_outcome = terminal_payload.and_then(decode_task_outcome);
    if store
        .finish_task_run(
            agent_run_id,
            status,
            terminal_payload,
            task_outcome.as_ref(),
            token_count,
            error,
        )
        .await
        .map_err(|err| AgentRunError::Internal(err.to_string()))?
        .is_some()
    {
        return Ok(());
    }

    let parented_outcome = terminal_payload.and_then(decode_parented_outcome);
    if store
        .finish_parented_run(
            agent_run_id,
            status,
            terminal_payload,
            parented_outcome.as_ref(),
            token_count,
            error,
        )
        .await
        .map_err(|err| AgentRunError::Internal(err.to_string()))?
        .is_some()
    {
        return Ok(());
    }

    Err(AgentRunError::Internal(format!(
        "task-agent-run row not updated for {}",
        agent_run_id.as_str()
    )))
}

pub(crate) fn completion_from_agent_run(
    agent_run_id: &AgentRunId,
    run: &AgentRun,
) -> Option<AgentRunOutcome> {
    run.finished_at?;
    if let Some(terminal) = &run.terminal_payload {
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

fn decode_task_outcome(payload: &JsonObject) -> Option<TaskOutcome> {
    serde_json::from_value(serde_json::Value::Object(payload.clone())).ok()
}

fn decode_parented_outcome(payload: &JsonObject) -> Option<ParentedOutcome> {
    serde_json::from_value(serde_json::Value::Object(payload.clone())).ok()
}
