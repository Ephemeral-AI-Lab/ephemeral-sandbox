use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::AgentType;
use eos_agent_message_records::AgentRunRecordKind;
use eos_agent_run::{
    AgentRunApi, AgentRunError, AgentRunOutcome, AgentRunStatus, SpawnAgentRequest,
};
use eos_llm_client::Message;
use eos_tool_core::{CommandSessionPort, SubagentSessionStatus, ToolResult};
use eos_state::AgentRun;
use eos_types::{AgentRunId, JsonObject};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::background::BackgroundTeardownPort;
use crate::runtime::AgentRunControlFactory;
use crate::{run_agent, AgentRunInput, EngineRunHandles};

#[derive(Clone)]
pub struct AgentRunService {
    handles: EngineRunHandles,
    control_factory: AgentRunControlFactory,
    runs: Arc<Mutex<HashMap<AgentRunId, SubagentRunHandle>>>,
}

#[derive(Clone)]
struct SubagentRunHandle {
    control: Arc<crate::AgentRunControl>,
    abort: AbortHandle,
    outcome_tx: watch::Sender<Option<AgentRunOutcome>>,
}

impl std::fmt::Debug for AgentRunService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunService").finish_non_exhaustive()
    }
}

impl AgentRunService {
    #[must_use]
    pub fn new(handles: EngineRunHandles, control_factory: AgentRunControlFactory) -> Self {
        Self {
            handles,
            control_factory,
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl AgentRunApi for AgentRunService {
    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> Result<AgentRunId, AgentRunError> {
        let registry = &self.handles.agent_registry;
        let requested_agent_name = request.agent_name.as_str().to_owned();
        let Some(agent_def) = registry.get(&request.agent_name) else {
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
        let agent_run_id = request.agent_run_id.unwrap_or_else(AgentRunId::new_v4);
        let control = self
            .control_factory
            .persisted(agent_run_id.clone(), request.task_id.clone());
        let background = control.background();
        let background_port: Arc<dyn BackgroundTeardownPort> = Arc::new(background.clone());
        let command_port: Arc<dyn CommandSessionPort> = Arc::new(background.clone());
        let agent_run_port: Arc<dyn AgentRunApi> = Arc::new(background.clone());
        let subagent_port: Arc<dyn eos_tool_core::SubagentSessionPort> =
            Arc::new(background.clone());
        let workflow_port: Arc<dyn eos_tool_core::WorkflowSessionPort> = Arc::new(background);
        let meta = eos_tool_core::ExecutionMetadata {
            agent_name: agent_def.name.as_str().to_owned(),
            agent_run_id: Some(agent_run_id.clone()),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            attempt_id: request.attempt_id.clone(),
            workflow_id: request.workflow_id.clone(),
            tool_use_id: None,
            sandbox_invocation_id: None,
            sandbox_id: request.sandbox_id.clone(),
            is_isolated_workspace_mode: request.is_isolated_workspace_mode,
            workspace_root: request.workspace_root,
            conversation: Arc::from(Vec::<Message>::new()),
        };

        let run_input = AgentRunInput {
            agent: agent_def,
            initial_messages: request.initial_messages,
            task_id: request.task_id,
            agent_run_id: agent_run_id.clone(),
            tool_metadata: meta,
            attempt_submission: None,
            agent_run_service: Some(agent_run_port),
            subagent_sessions: Some(subagent_port),
            workflow_service: None,
            workflow_sessions: Some(workflow_port),
            background_session: Some(background_port),
            command_session_port: Some(command_port),
            notifier: control.notifications(),
            cancellation: control.cancellation(),
            foreground: control.foreground(),
            agent_run_registry: None,
            persist_agent_run: request.persist,
            record_kind: request.record_kind,
        };

        let (outcome_tx, _) = watch::channel(None);
        let publish_tx = outcome_tx.clone();
        let handles = self.handles.clone();
        let spawned_agent_run_id = agent_run_id.clone();
        let join = tokio::spawn(async move {
            let run = run_agent(&handles, run_input, None).await;
            let outcome = outcome_from_run(spawned_agent_run_id, run);
            let _ = publish_tx.send(Some(outcome));
        });
        self.runs.lock().await.insert(
            agent_run_id.clone(),
            SubagentRunHandle {
                control,
                abort: join.abort_handle(),
                outcome_tx,
            },
        );

        Ok(agent_run_id)
    }

    async fn wait_for_agent_outcomes(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        if let Some(outcome) = self.poll_agent_run_outcome(agent_run_id).await? {
            return Ok(outcome);
        }
        let mut rx = {
            let guard = self.runs.lock().await;
            guard
                .get(agent_run_id)
                .map(|handle| handle.outcome_tx.subscribe())
                .ok_or_else(|| AgentRunError::NotActiveInProcess(agent_run_id.clone()))?
        };
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
        if let Some(outcome) = self
            .runs
            .lock()
            .await
            .get(agent_run_id)
            .and_then(|handle| handle.outcome_tx.borrow().clone())
        {
            return Ok(Some(outcome));
        }
        let Some(run) = self
            .handles
            .agent_run_store
            .get(agent_run_id)
            .await
            .map_err(|err| AgentRunError::Internal(err.to_string()))?
        else {
            return Ok(None);
        };
        Ok(completion_from_agent_run(&run).map(|(status, result)| AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: subagent_status_to_agent_run_status(status),
            terminal_result: Some(result),
            terminal_payload: run.terminal_tool_result.clone(),
            message_history: Vec::new(),
            token_count: Some(run.token_count),
            error: run.error.clone(),
        }))
    }

    async fn cancel_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> Result<(), AgentRunError> {
        let Some(handle) = self.runs.lock().await.remove(agent_run_id) else {
            return Ok(());
        };
        handle
            .control
            .finalization()
            .finish_cancelled(reason)
            .await
            .map_err(|err| AgentRunError::Internal(err.to_string()))?;
        handle.abort.abort();
        let _ = handle.outcome_tx.send(Some(AgentRunOutcome {
            agent_run_id: agent_run_id.clone(),
            status: AgentRunStatus::Cancelled,
            terminal_result: Some(
                ToolResult::error(format!("agent run cancelled: {reason}"))
                    .meta("agent_run_cancelled", json!(true)),
            ),
            terminal_payload: None,
            message_history: Vec::new(),
            token_count: None,
            error: Some(reason.to_owned()),
        }));
        Ok(())
    }
}

fn outcome_from_run(agent_run_id: AgentRunId, run: crate::AgentRunResult) -> AgentRunOutcome {
    let missing_terminal = run.error.is_none() && run.terminal_result.is_none();
    let error = run.error.or_else(|| {
        missing_terminal.then(|| "agent exited without calling a terminal tool".to_owned())
    });
    AgentRunOutcome {
        agent_run_id,
        status: if error.is_some() {
            AgentRunStatus::Failed
        } else {
            AgentRunStatus::Completed
        },
        terminal_result: run.terminal_result,
        terminal_payload: None,
        message_history: Vec::new(),
        token_count: None,
        error,
    }
}

const fn subagent_status_to_agent_run_status(status: SubagentSessionStatus) -> AgentRunStatus {
    match status {
        SubagentSessionStatus::Completed | SubagentSessionStatus::Delivered => {
            AgentRunStatus::Completed
        }
        SubagentSessionStatus::Cancelled => AgentRunStatus::Cancelled,
        SubagentSessionStatus::Running | SubagentSessionStatus::Failed => AgentRunStatus::Failed,
    }
}

const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
    }
}

fn completion_from_agent_run(run: &AgentRun) -> Option<(SubagentSessionStatus, ToolResult)> {
    run.finished_at?;
    if let Some(terminal) = &run.terminal_tool_result {
        return Some((
            SubagentSessionStatus::Completed,
            tool_result_from_payload(terminal),
        ));
    }
    let message = match &run.error {
        Some(error) => format!("subagent crashed: {error}"),
        None => "subagent exited without calling a terminal tool. Findings were not delivered."
            .to_owned(),
    };
    Some((
        SubagentSessionStatus::Failed,
        ToolResult::error(message).meta("subagent_terminal_called", json!(false)),
    ))
}

fn tool_result_from_payload(payload: &JsonObject) -> ToolResult {
    let output = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let is_error = payload
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_terminal = payload
        .get("is_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut metadata = payload
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert("subagent_terminal_called".to_owned(), json!(true));
    ToolResult {
        output,
        is_error,
        metadata,
        is_terminal,
    }
}
