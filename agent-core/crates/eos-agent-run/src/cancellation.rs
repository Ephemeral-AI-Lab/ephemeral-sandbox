//! Agent-run cancellation orchestration.

use eos_types::{
    AgentRunError, AgentRunId, AgentRunOutcome, AgentRunStatus, JsonObject, TaskStatus,
};

use crate::persistence::{finish_cancelled_agent_run, finish_task_agent_run};
use crate::service::AgentRunService;

pub(crate) async fn cancel_agent_run(
    service: &AgentRunService,
    agent_run_id: &AgentRunId,
    reason: &str,
) -> Result<(), AgentRunError> {
    let completion = service.active_agent_runs.take(agent_run_id).await;
    if let Some(completion) = &completion {
        debug_assert_eq!(completion.agent_run_id(), agent_run_id);
        completion.cancel(reason);
    }

    let finish = finish_cancelled_agent_run(&*service.agent_run_store, agent_run_id, reason).await;
    let payload = cancelled_task_payload(reason);
    let finish_lineage = finish_task_agent_run(
        &*service.task_agent_run_store,
        agent_run_id,
        TaskStatus::Cancelled,
        Some(&payload),
        0,
        Some(reason),
    )
    .await;
    let finalization_error = finish.and(finish_lineage).err();
    let outcome = match &finalization_error {
        Some(err) => AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Failed,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(err.to_string()),
        },
        None => AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Cancelled,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(reason.to_owned()),
        },
    };
    if let Some(completion) = completion {
        completion.publish(outcome);
    }
    if let Some(err) = finalization_error {
        Err(err)
    } else {
        Ok(())
    }
}

fn cancelled_task_payload(reason: &str) -> JsonObject {
    let mut payload = JsonObject::new();
    payload.insert("fail_reason".to_owned(), serde_json::json!("cancelled"));
    payload.insert("reason".to_owned(), serde_json::json!(reason));
    payload
}
