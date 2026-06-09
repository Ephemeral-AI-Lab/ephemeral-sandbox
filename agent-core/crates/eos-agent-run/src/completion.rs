//! Engine-completion handoff and caller wait/poll publication.

use eos_types::{
    AgentLoopCompletion, AgentLoopMessage, AgentLoopOutcome, AgentLoopOutcomeKind, AgentRunError,
    AgentRunId, AgentRunOutcome, AgentRunStatus, Message, TaskStatus,
};

use crate::persistence::{completion_from_agent_run, finish_agent_run, finish_task_agent_run};
use crate::service::AgentRunService;

pub(crate) fn spawn_forwarder(
    service: AgentRunService,
    agent_run_id: AgentRunId,
    loop_completion: AgentLoopCompletion,
) {
    tokio::spawn(async move {
        forward_agent_loop_outcome(service, agent_run_id, loop_completion).await;
    });
}

pub(crate) async fn wait_for_agent_outcome(
    service: &AgentRunService,
    agent_run_id: &AgentRunId,
) -> Result<AgentRunOutcome, AgentRunError> {
    if let Some(outcome) = poll_agent_run_outcome(service, agent_run_id).await? {
        return Ok(outcome);
    }
    let mut rx = service.active_agent_runs.subscribe(agent_run_id).await?;
    loop {
        if let Some(outcome) = rx.borrow().clone() {
            return Ok(outcome);
        }
        rx.changed()
            .await
            .map_err(|_| AgentRunError::CompletionChannelClosed(agent_run_id.clone()))?;
    }
}

pub(crate) async fn poll_agent_run_outcome(
    service: &AgentRunService,
    agent_run_id: &AgentRunId,
) -> Result<Option<AgentRunOutcome>, AgentRunError> {
    if let Some(outcome) = service
        .active_agent_runs
        .current_outcome(agent_run_id)
        .await
    {
        return Ok(Some(outcome));
    }
    let Some(run) = service
        .agent_run_store
        .get(agent_run_id)
        .await
        .map_err(|err| AgentRunError::Internal(err.to_string()))?
    else {
        return Ok(None);
    };
    Ok(completion_from_agent_run(agent_run_id, &run))
}

async fn forward_agent_loop_outcome(
    service: AgentRunService,
    agent_run_id: AgentRunId,
    loop_completion: AgentLoopCompletion,
) {
    let loop_outcome = loop_completion.wait().await;
    let Some(completion) = service.active_agent_runs.take(&agent_run_id).await else {
        return;
    };
    let outcome =
        finalize_agent_run_from_agent_loop_outcome(&service, agent_run_id.clone(), loop_outcome)
            .await;
    completion.publish(outcome);
    if let Some(runtime_state) = &service.runtime_state {
        runtime_state.remove_runtime_state(&agent_run_id);
    }
}

async fn finalize_agent_run_from_agent_loop_outcome(
    service: &AgentRunService,
    agent_run_id: AgentRunId,
    outcome: AgentLoopOutcome,
) -> AgentRunOutcome {
    let agent_outcome = agent_run_outcome_from_loop(agent_run_id.clone(), outcome);
    let error = agent_outcome.error.as_deref();
    let finish = finish_agent_run(
        &*service.agent_run_store,
        &agent_run_id,
        agent_outcome.submission_payload.as_ref(),
        agent_outcome.token_count,
        error,
    )
    .await;
    let finish_lineage = finish_task_agent_run(
        &*service.task_agent_run_store,
        &agent_run_id,
        task_status_for_agent_status(agent_outcome.status),
        agent_outcome.submission_payload.as_ref(),
        agent_outcome.token_count.unwrap_or_default(),
        error,
    )
    .await;
    match finish.and(finish_lineage) {
        Ok(()) => agent_outcome,
        Err(err) => AgentRunOutcome {
            agent_run_id,
            status: AgentRunStatus::Failed,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(err.to_string()),
        },
    }
}

fn agent_run_outcome_from_loop(
    agent_run_id: AgentRunId,
    outcome: AgentLoopOutcome,
) -> AgentRunOutcome {
    let message_history = loop_messages_to_llm_messages(outcome.final_conversation_messages);
    let total_token_count = outcome.total_token_count;
    match outcome.kind {
        AgentLoopOutcomeKind::TerminalToolSubmitted { submission_payload } => AgentRunOutcome {
            agent_run_id,
            status: AgentRunStatus::Completed,
            submission_payload: Some(submission_payload),
            message_history,
            token_count: total_token_count,
            error: None,
        },
        AgentLoopOutcomeKind::LoopFailed { error_summary } => AgentRunOutcome {
            agent_run_id,
            status: AgentRunStatus::Failed,
            submission_payload: None,
            message_history,
            token_count: total_token_count,
            error: Some(error_summary),
        },
    }
}

fn loop_messages_to_llm_messages(messages: Vec<AgentLoopMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|message| match message {
            AgentLoopMessage::SystemPrompt(_) => None,
            AgentLoopMessage::UserMessage(message)
            | AgentLoopMessage::AssistantMessage(message) => Some(message),
        })
        .collect()
}

const fn task_status_for_agent_status(status: AgentRunStatus) -> TaskStatus {
    match status {
        AgentRunStatus::Completed => TaskStatus::Done,
        AgentRunStatus::Failed => TaskStatus::Failed,
        AgentRunStatus::Cancelled => TaskStatus::Cancelled,
    }
}
