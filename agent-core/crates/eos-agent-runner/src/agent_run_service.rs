//! Launcher-backed agent-run lifecycle service.

use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentName as DefinitionAgentName, AgentRegistry, AgentType};
use eos_agent_ports::{
    AgentLoopLauncher, AgentLoopOutcome, AgentLoopOutcomeKind, AgentRunApi, AgentRunError,
    AgentRunOutcome, AgentRunRecordKind, AgentRunStatus, SpawnAgentRequest,
};
use eos_llm_client::Message;
use eos_types::{AgentRunId, AgentRunStore};
use tokio::sync::oneshot;

use crate::active_agent_runs::ActiveAgentRuns;
use crate::agent_loop_request::build_start_agent_loop_request;
use crate::agent_run_persistence::{
    completion_from_agent_run, create_agent_run_if_requested, finish_agent_run_cancelled,
    finish_agent_run_if_requested, tool_result_payload,
};

/// Agent-run lifecycle service.
#[derive(Clone)]
pub struct AgentRunService {
    agent_registry: Arc<AgentRegistry>,
    agent_loop_launcher: Arc<dyn AgentLoopLauncher>,
    agent_run_store: Arc<dyn AgentRunStore>,
    active_agent_runs: ActiveAgentRuns,
}

impl std::fmt::Debug for AgentRunService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunService").finish_non_exhaustive()
    }
}

impl AgentRunService {
    /// Build a runner service from contract ports.
    #[must_use]
    pub fn new(
        agent_registry: Arc<AgentRegistry>,
        agent_loop_launcher: Arc<dyn AgentLoopLauncher>,
        agent_run_store: Arc<dyn AgentRunStore>,
    ) -> Self {
        Self {
            agent_registry,
            agent_loop_launcher,
            agent_run_store,
            active_agent_runs: ActiveAgentRuns::new(),
        }
    }

    async fn finalize_agent_run_from_agent_loop_outcome(
        &self,
        agent_run_id: AgentRunId,
        persistence_requested: bool,
        outcome: AgentLoopOutcome,
    ) -> AgentRunOutcome {
        let agent_outcome = agent_run_outcome_from_loop(agent_run_id.clone(), outcome);
        let submission = agent_outcome
            .submission_payload
            .as_ref()
            .map(tool_result_from_payload);
        let error = agent_outcome.error.as_deref();
        let finish = finish_agent_run_if_requested(
            &*self.agent_run_store,
            persistence_requested,
            &agent_run_id,
            submission.as_ref(),
            agent_outcome.token_count,
            error,
        )
        .await;
        match finish {
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

    async fn finalize_agent_run_from_dropped_agent_loop_sender(
        &self,
        agent_run_id: AgentRunId,
        persistence_requested: bool,
    ) -> AgentRunOutcome {
        let error = "agent loop outcome sender dropped".to_owned();
        let _ignored = finish_agent_run_if_requested(
            &*self.agent_run_store,
            persistence_requested,
            &agent_run_id,
            None,
            None,
            Some(&error),
        )
        .await;
        AgentRunOutcome {
            agent_run_id,
            status: AgentRunStatus::Failed,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(error),
        }
    }
}

#[async_trait]
impl AgentRunApi for AgentRunService {
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> Result<AgentRunId, AgentRunError> {
        let requested_agent_name = request.agent_name.as_str().to_owned();
        let agent_name = DefinitionAgentName::new(request.agent_name.as_str())
            .map_err(|_| AgentRunError::AgentNotRegistered(requested_agent_name.clone()))?;
        let Some(agent_def) = self.agent_registry.get(&agent_name) else {
            return Err(AgentRunError::AgentNotRegistered(requested_agent_name));
        };
        if matches!(request.record_kind, AgentRunRecordKind::Subagent { .. })
            && agent_def.agent_type != AgentType::Subagent
        {
            return Err(AgentRunError::WrongAgentType {
                agent_name: requested_agent_name,
                expected: "subagent",
                actual: agent_type_value(agent_def.agent_type),
            });
        }

        let agent_def = (**agent_def).clone();
        let agent_run_id = request
            .agent_run_id
            .clone()
            .unwrap_or_else(AgentRunId::new_v4);
        let persistence_requested = create_agent_run_if_requested(
            &*self.agent_run_store,
            request.persist,
            request.task_id.as_ref(),
            &agent_run_id,
            agent_def.name.as_str(),
        )
        .await?;
        let start_request =
            build_start_agent_loop_request(&agent_def, request, agent_run_id.clone());
        let started = self.agent_loop_launcher.start_agent_loop(start_request);

        self.active_agent_runs
            .insert(agent_run_id.clone(), started.cancel_handle)
            .await;
        let service = self.clone();
        let forward_agent_run_id = agent_run_id.clone();
        tokio::spawn(async move {
            forward_agent_loop_outcome(
                service,
                forward_agent_run_id,
                persistence_requested,
                started.outcome_receiver,
            )
            .await;
        });

        Ok(agent_run_id)
    }

    async fn wait_for_agent_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        if let Some(outcome) = self.poll_agent_run_outcome(agent_run_id).await? {
            return Ok(outcome);
        }
        let mut rx = self.active_agent_runs.subscribe(agent_run_id).await?;
        loop {
            if let Some(outcome) = rx.borrow().clone() {
                return Ok(outcome);
            }
            rx.changed()
                .await
                .map_err(|_| AgentRunError::CompletionChannelClosed(agent_run_id.clone()))?;
        }
    }

    async fn poll_agent_run_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        if let Some(outcome) = self.active_agent_runs.current_outcome(agent_run_id).await {
            return Ok(Some(outcome));
        }
        let Some(run) = self
            .agent_run_store
            .get(agent_run_id)
            .await
            .map_err(|err| AgentRunError::Internal(err.to_string()))?
        else {
            return Ok(None);
        };
        Ok(completion_from_agent_run(agent_run_id, &run))
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError> {
        let completion = self.active_agent_runs.take(agent_run_id).await;
        if let Some(completion) = &completion {
            completion.cancel(reason);
        }
        finish_agent_run_cancelled(&*self.agent_run_store, agent_run_id, reason).await?;
        let outcome = AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Cancelled,
            submission_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(reason.to_owned()),
        };
        if let Some(completion) = completion {
            completion.publish(outcome);
        }
        Ok(())
    }
}

async fn forward_agent_loop_outcome(
    service: AgentRunService,
    agent_run_id: AgentRunId,
    persistence_requested: bool,
    outcome_receiver: oneshot::Receiver<AgentLoopOutcome>,
) {
    let received = outcome_receiver.await;
    let Some(completion) = service.active_agent_runs.take(&agent_run_id).await else {
        return;
    };
    let outcome = match received {
        Ok(outcome) => {
            service
                .finalize_agent_run_from_agent_loop_outcome(
                    agent_run_id.clone(),
                    persistence_requested,
                    outcome,
                )
                .await
        }
        Err(_closed) => {
            service
                .finalize_agent_run_from_dropped_agent_loop_sender(
                    agent_run_id.clone(),
                    persistence_requested,
                )
                .await
        }
    };
    completion.publish(outcome);
}

fn agent_run_outcome_from_loop(
    agent_run_id: AgentRunId,
    outcome: AgentLoopOutcome,
) -> AgentRunOutcome {
    let message_history = loop_messages_to_llm_messages(outcome.final_conversation_messages);
    let total_token_count = outcome.total_token_count;
    match outcome.kind {
        AgentLoopOutcomeKind::TerminalToolSubmitted {
            outcome: tool_result,
        } => AgentRunOutcome {
            agent_run_id,
            status: AgentRunStatus::Completed,
            submission_payload: Some(tool_result_payload(&tool_result)),
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

fn loop_messages_to_llm_messages(messages: Vec<eos_agent_ports::AgentLoopMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|message| match message {
            eos_agent_ports::AgentLoopMessage::SystemPrompt(_) => None,
            eos_agent_ports::AgentLoopMessage::UserMessage(message)
            | eos_agent_ports::AgentLoopMessage::AssistantMessage(message) => Some(message),
        })
        .collect()
}

fn tool_result_from_payload(payload: &eos_types::JsonObject) -> eos_tool_ports::ToolResult {
    let output = payload
        .get("output")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_error = payload
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let metadata = payload
        .get("metadata")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    eos_tool_ports::ToolResult {
        output,
        is_error,
        metadata,
        is_terminal: false,
    }
}

const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
    }
}
