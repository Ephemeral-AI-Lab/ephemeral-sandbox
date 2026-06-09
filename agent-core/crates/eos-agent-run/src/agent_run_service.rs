//! Launcher-backed agent-run lifecycle service.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{
    AgentDefinition, AgentLoopLauncher, AgentLoopMessage, AgentLoopOutcome, AgentLoopOutcomeFuture,
    AgentLoopOutcomeKind, AgentName as DefinitionAgentName, AgentRegistry, AgentRunApi,
    AgentRunError, AgentRunId, AgentRunOutcome, AgentRunStatus, AgentRunStore, AgentType, Message,
    ParentedAgentRunKind, SpawnAgentRequest, TaskAgentRunKind,
};

use crate::active_agent_runs::{ActiveAgentRunRecord, ActiveAgentRuns};
use crate::agent_loop_request::build_start_agent_loop_request;
use crate::agent_run_persistence::{
    completion_from_agent_run, create_agent_run_if_requested, finish_agent_run_cancelled,
    finish_agent_run_if_requested,
};
use crate::agent_run_records::to_agent_run_record_kind;
use crate::records::{AgentMessageRecords, AgentRunRecordStart, NodeFinishStatus};

type RuntimeStateRecorder =
    Arc<dyn Fn(&SpawnAgentRequest, &AgentRunId) -> Result<(), AgentRunError> + Send + Sync>;
type RuntimeStateRemover = Arc<dyn Fn(&AgentRunId) + Send + Sync>;

/// Agent-run lifecycle service.
#[derive(Clone)]
pub struct AgentRunService {
    agent_registry: Arc<AgentRegistry>,
    agent_loop_launcher: Arc<dyn AgentLoopLauncher>,
    agent_run_store: Arc<dyn AgentRunStore>,
    message_records: Option<AgentMessageRecords>,
    active_agent_runs: ActiveAgentRuns,
    runtime_state_recorder: Option<RuntimeStateRecorder>,
    runtime_state_remover: Option<RuntimeStateRemover>,
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
        message_records: Option<AgentMessageRecords>,
    ) -> Self {
        Self {
            agent_registry,
            agent_loop_launcher,
            agent_run_store,
            message_records,
            active_agent_runs: ActiveAgentRuns::new(),
            runtime_state_recorder: None,
            runtime_state_remover: None,
        }
    }

    /// Attach runtime-only state hooks used by the production composition layer.
    ///
    /// The runner still owns agent-run lifecycle state; these hooks only record
    /// and remove mutable execution facts such as workspace/isolation metadata.
    #[must_use]
    pub fn with_runtime_state_hooks<Record, Remove>(
        mut self,
        record: Record,
        remove: Remove,
    ) -> Self
    where
        Record: Fn(&SpawnAgentRequest, &AgentRunId) -> Result<(), AgentRunError>
            + Send
            + Sync
            + 'static,
        Remove: Fn(&AgentRunId) + Send + Sync + 'static,
    {
        self.runtime_state_recorder = Some(Arc::new(record));
        self.runtime_state_remover = Some(Arc::new(remove));
        self
    }

    async fn start_message_record(
        &self,
        request: &SpawnAgentRequest,
        agent_def: &AgentDefinition,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<ActiveAgentRunRecord>, AgentRunError> {
        let Some(message_records) = &self.message_records else {
            return Ok(None);
        };
        let request_id = request.target.request_id();
        let kind = to_agent_run_record_kind(&request.target.task_agent_run_kind());
        let handle = message_records
            .start_agent_run(AgentRunRecordStart {
                request_id,
                task_id: request.target.current_task_id(),
                agent_run_id,
                agent_name: agent_def.name.as_str(),
                kind: &kind,
                system_prompt: agent_def.system_prompt.as_deref().unwrap_or_default(),
                initial_messages: &request.initial_messages,
            })
            .await
            .map_err(|err| AgentRunError::Internal(err.to_string()))?;
        Ok(Some(ActiveAgentRunRecord::new(
            handle,
            request.initial_messages.len(),
        )))
    }

    async fn finish_message_record(
        &self,
        message_record: Option<ActiveAgentRunRecord>,
        outcome: &AgentRunOutcome,
    ) {
        let Some(message_record) = message_record else {
            return;
        };
        let later_message_start = message_record
            .initial_message_count
            .min(outcome.message_history.len());
        if let Err(err) = message_record
            .handle
            .append_messages(&outcome.message_history[later_message_start..])
            .await
        {
            tracing::warn!(
                agent_run_id = %outcome.agent_run_id,
                error = %err,
                "failed to append agent-run message record messages"
            );
        }

        let status = match outcome.status {
            AgentRunStatus::Completed => NodeFinishStatus::Completed,
            AgentRunStatus::Failed | AgentRunStatus::Cancelled => NodeFinishStatus::Failed,
        };
        if let Err(err) = message_record.handle.finish(status).await {
            tracing::warn!(
                agent_run_id = %outcome.agent_run_id,
                error = %err,
                "failed to finish agent-run message record"
            );
        }
    }

    async fn finalize_agent_run_from_agent_loop_outcome(
        &self,
        agent_run_id: AgentRunId,
        persistence_requested: bool,
        outcome: AgentLoopOutcome,
    ) -> AgentRunOutcome {
        let agent_outcome = agent_run_outcome_from_loop(agent_run_id.clone(), outcome);
        let error = agent_outcome.error.as_deref();
        let finish = finish_agent_run_if_requested(
            &*self.agent_run_store,
            persistence_requested,
            &agent_run_id,
            agent_outcome.submission_payload.as_ref(),
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
        let expected = expected_agent_type(&request.target.task_agent_run_kind());
        if agent_def.agent_type != expected {
            return Err(AgentRunError::WrongAgentType {
                agent_name: requested_agent_name,
                expected: agent_type_value(expected),
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
            request.target.current_task_id(),
            &agent_run_id,
            agent_def.name.as_str(),
        )
        .await?;
        let message_record = self
            .start_message_record(&request, &agent_def, &agent_run_id)
            .await?;
        if let Some(record_runtime_state) = &self.runtime_state_recorder {
            record_runtime_state(&request, &agent_run_id)?;
        }
        let start_request =
            build_start_agent_loop_request(&agent_def, request, agent_run_id.clone());
        let agent_run_api: Arc<dyn AgentRunApi> = Arc::new(self.clone());
        let started = self
            .agent_loop_launcher
            .start_agent_loop(start_request, agent_run_api);

        self.active_agent_runs
            .insert(agent_run_id.clone(), started.cancellation, message_record)
            .await;
        let service = self.clone();
        let forward_agent_run_id = agent_run_id.clone();
        tokio::spawn(async move {
            forward_agent_loop_outcome(
                service,
                forward_agent_run_id,
                persistence_requested,
                started.outcome,
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
        let mut completion = self.active_agent_runs.take(agent_run_id).await;
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
        if let Some(completion) = &mut completion {
            self.finish_message_record(completion.take_message_record(), &outcome)
                .await;
        }
        if let Some(completion) = completion {
            completion.publish(outcome);
        }
        if let Some(remove_runtime_state) = &self.runtime_state_remover {
            remove_runtime_state(agent_run_id);
        }
        Ok(())
    }
}

async fn forward_agent_loop_outcome(
    service: AgentRunService,
    agent_run_id: AgentRunId,
    persistence_requested: bool,
    outcome: AgentLoopOutcomeFuture,
) {
    let received = outcome.await;
    let Some(mut completion) = service.active_agent_runs.take(&agent_run_id).await else {
        return;
    };
    let outcome = match received {
        Some(outcome) => {
            service
                .finalize_agent_run_from_agent_loop_outcome(
                    agent_run_id.clone(),
                    persistence_requested,
                    outcome,
                )
                .await
        }
        None => {
            service
                .finalize_agent_run_from_dropped_agent_loop_sender(
                    agent_run_id.clone(),
                    persistence_requested,
                )
                .await
        }
    };
    service
        .finish_message_record(completion.take_message_record(), &outcome)
        .await;
    completion.publish(outcome);
    if let Some(remove_runtime_state) = &service.runtime_state_remover {
        remove_runtime_state(&agent_run_id);
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

const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
        AgentType::Advisor => "advisor",
    }
}

const fn expected_agent_type(task_agent_run_kind: &TaskAgentRunKind) -> AgentType {
    match task_agent_run_kind {
        TaskAgentRunKind::Root | TaskAgentRunKind::Workflow { .. } => AgentType::Agent,
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Subagent,
            ..
        } => AgentType::Subagent,
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Advisor,
            ..
        } => AgentType::Advisor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_agent_run_kind_declares_required_agent_type() {
        let parent_agent_run_id = AgentRunId::new_v4();
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Root),
            AgentType::Agent
        );
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Parented {
                parent_agent_run_id: parent_agent_run_id.clone(),
                kind: ParentedAgentRunKind::Subagent,
            }),
            AgentType::Subagent
        );
        assert_eq!(
            expected_agent_type(&TaskAgentRunKind::Parented {
                parent_agent_run_id,
                kind: ParentedAgentRunKind::Advisor,
            }),
            AgentType::Advisor
        );
    }
}
