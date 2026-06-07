use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentName, AgentType};
use eos_agent_message_records::AgentRunRecordKind;
use eos_llm_client::Message;
use eos_tools::ports::{
    BackgroundSupervisorPort, CommandSessionSupervisorPort, SpawnedSubagent, StartedSubagent,
    SubagentLaunch, SubagentLaunchRejection,
};
use eos_tools::{ExecutionMetadata, ToolError, ToolResult};
use eos_types::{AgentRunId, JsonObject, SubagentSessionId};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use super::session::SubagentSession;
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};
use crate::runtime::AgentRunControlFactory;
use crate::{run_agent, AgentRunInput, AgentRunResult, EngineRunHandles};

#[derive(Debug, Clone)]
pub(in crate::background) struct SubagentCompletion {
    pub(super) subagent_session_id: SubagentSessionId,
    pub(super) status: BackgroundSessionStatus,
    pub(super) result: ToolResult,
}

#[derive(Debug, Clone)]
pub(in crate::background) struct SubagentProgressSnapshot {
    pub(in crate::background) status: BackgroundSessionStatus,
    pub(in crate::background) result: Option<ToolResult>,
    pub(in crate::background) agent_name: String,
}

#[derive(Default)]
struct SubagentSessionState {
    next_session_seq: u64,
    sessions: HashMap<SubagentSessionId, SubagentSession>,
}

/// Tracks subagent background sessions for one agent run.
#[derive(Clone)]
pub(in crate::background) struct SubagentSessionManager {
    sessions: Arc<Mutex<SubagentSessionState>>,
    handles: EngineRunHandles,
    control_factory: AgentRunControlFactory,
    notification: BackgroundNotificationEmitter,
}

impl std::fmt::Debug for SubagentSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentSessionManager")
            .finish_non_exhaustive()
    }
}

impl SubagentSessionManager {
    pub(in crate::background) fn new(
        handles: EngineRunHandles,
        control_factory: AgentRunControlFactory,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(SubagentSessionState::default())),
            handles,
            control_factory,
            notification,
        }
    }

    pub(in crate::background) async fn spawn(
        &self,
        ctx: &ExecutionMetadata,
        launch: SubagentLaunch,
    ) -> Result<SpawnedSubagent, ToolError> {
        let registry = &self.handles.agent_registry;

        if let Ok(caller) = AgentName::new(ctx.agent_name.as_str()) {
            if registry.get(&caller).map(|def| def.agent_type) == Some(AgentType::Subagent) {
                return Ok(SpawnedSubagent::Rejected(
                    SubagentLaunchRejection::Recursive,
                ));
            }
        }

        let requested_agent_name = launch.agent_name.clone();
        let Ok(target) = AgentName::new(&requested_agent_name) else {
            return Ok(SpawnedSubagent::Rejected(
                SubagentLaunchRejection::NotRegistered {
                    agent_name: requested_agent_name,
                },
            ));
        };
        let Some(sub_def) = registry.get(&target) else {
            return Ok(SpawnedSubagent::Rejected(
                SubagentLaunchRejection::NotRegistered {
                    agent_name: requested_agent_name,
                },
            ));
        };
        if sub_def.agent_type != AgentType::Subagent {
            return Ok(SpawnedSubagent::Rejected(
                SubagentLaunchRejection::NotSubagent {
                    agent_name: requested_agent_name,
                    agent_type: agent_type_value(sub_def.agent_type).to_owned(),
                },
            ));
        }
        let sub_def = (**sub_def).clone();
        let SubagentLaunch {
            agent_name,
            prompt,
            guidance,
        } = launch;

        let caller_agent_run_id = ctx.require_agent_run_id()?.clone();
        let mut tool_input = JsonObject::new();
        tool_input.insert("agent_name".to_owned(), json!(agent_name.clone()));
        tool_input.insert("prompt".to_owned(), json!(prompt.clone()));

        let child_run_id = AgentRunId::new_v4();
        let subagent_control = self.control_factory.ephemeral(child_run_id.clone());
        let child_background = subagent_control.background();
        let child_background_port: Arc<dyn BackgroundSupervisorPort> =
            Arc::new(child_background.clone());
        let child_command_port: Arc<dyn CommandSessionSupervisorPort> = Arc::new(child_background);
        let mut child_meta = ctx.clone();
        child_meta.agent_name = sub_def.name.as_str().to_owned();
        child_meta.agent_run_id = Some(child_run_id.clone());
        child_meta.conversation = Arc::from(Vec::<Message>::new());
        child_meta.tool_use_id = None;

        let run_input = AgentRunInput {
            agent: sub_def,
            initial_messages: vec![
                Message::from_user_text(prompt),
                Message::from_user_text(guidance),
            ],
            task_id: None,
            agent_run_id: child_run_id.clone(),
            tool_metadata: child_meta,
            attempt_submission: None,
            workflow_control: None,
            background_supervisor: Some(child_background_port),
            command_session_supervisor: Some(child_command_port),
            notifier: subagent_control.notifications(),
            cancellation: subagent_control.cancellation(),
            foreground: subagent_control.foreground(),
            agent_run_registry: None,
            persist_agent_run: false,
            record_kind: AgentRunRecordKind::Subagent {
                parent_agent_run_id: caller_agent_run_id.clone(),
            },
        };

        let handles = self.handles.clone();
        let driver_manager = self.clone();
        let driver_agent_run_id = caller_agent_run_id.clone();
        let subagent_session_id = self.next_session_id().await;
        trace_background_tool(
            "background_tool.started",
            &subagent_session_id,
            &caller_agent_run_id,
            BackgroundSessionStatus::Running,
            None,
        );

        let driver_task_id = subagent_session_id.clone();
        let join = tokio::spawn(async move {
            let _subagent_control = subagent_control;
            let run = run_agent(&handles, run_input, None).await;
            let (status, result, exit_code) = classify_run(run);
            if let Some(completion) = driver_manager.settle(&driver_task_id, status, result).await {
                driver_manager.finish(completion).await;
            }
            trace_background_tool(
                terminal_event_type(status),
                &driver_task_id,
                &driver_agent_run_id,
                status,
                Some(exit_code),
            );
        });

        self.insert(SubagentSession::running(
            subagent_session_id.clone(),
            child_run_id,
            join.abort_handle(),
            tool_input,
        ))
        .await;

        Ok(SpawnedSubagent::Launched(StartedSubagent {
            subagent_session_id,
        }))
    }

    pub(in crate::background) async fn progress_snapshot(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<SubagentProgressSnapshot> {
        let guard = self.sessions.lock().await;
        let session = guard.sessions.get(subagent_session_id)?;
        let agent_name = session
            .tool_input()
            .get("agent_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Some(SubagentProgressSnapshot {
            status: session.status(),
            result: session.result().cloned(),
            agent_name,
        })
    }

    pub(in crate::background) async fn cancel_one(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> bool {
        let mut guard = self.sessions.lock().await;
        guard
            .sessions
            .get_mut(subagent_session_id)
            .is_some_and(|session| session.cancel(reason))
    }

    async fn next_session_id(&self) -> SubagentSessionId {
        let mut guard = self.sessions.lock().await;
        guard.next_session_seq = guard.next_session_seq.saturating_add(1);
        match format!("subagent_{}", guard.next_session_seq).parse() {
            Ok(id) => id,
            Err(_) => unreachable!("generated subagent ids are non-empty"),
        }
    }

    pub(super) async fn settle(
        &self,
        subagent_session_id: &SubagentSessionId,
        status: BackgroundSessionStatus,
        result: ToolResult,
    ) -> Option<SubagentCompletion> {
        let mut guard = self.sessions.lock().await;
        let session = guard.sessions.get_mut(subagent_session_id)?;
        let result = session.settle(status, result)?;
        Some(SubagentCompletion {
            subagent_session_id: subagent_session_id.clone(),
            status: session.status(),
            result,
        })
    }
}

#[async_trait]
impl BackgroundSessionManager for SubagentSessionManager {
    type Session = SubagentSession;
    type Completion = SubagentCompletion;

    async fn insert(&self, session: Self::Session) {
        self.sessions
            .lock()
            .await
            .sessions
            .insert(session.id().clone(), session);
    }

    async fn count(&self) -> usize {
        self.sessions
            .lock()
            .await
            .sessions
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .count()
    }

    async fn poll(&self) -> Vec<Self::Completion> {
        Vec::new()
    }

    async fn finish(&self, completion: Self::Completion) {
        let _ = self
            .notification
            .emit(BackgroundCompletion::Subagent {
                subagent_session_id: completion.subagent_session_id,
                status: completion.status,
                result: completion.result,
            })
            .await;
    }

    async fn cancel(&self, reason: &str) {
        for session in self.sessions.lock().await.sessions.values_mut() {
            let _ = session.cancel(reason);
        }
    }
}

const fn agent_type_value(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Agent => "agent",
        AgentType::Subagent => "subagent",
    }
}

const fn terminal_event_type(status: BackgroundSessionStatus) -> &'static str {
    match status {
        BackgroundSessionStatus::Running => "background_tool.started",
        BackgroundSessionStatus::Completed => "background_tool.completed",
        BackgroundSessionStatus::Failed => "background_tool.failed",
        BackgroundSessionStatus::Cancelled => "background_tool.cancelled",
        BackgroundSessionStatus::Delivered => "background_tool.delivered",
    }
}

const fn status_value(status: BackgroundSessionStatus) -> &'static str {
    match status {
        BackgroundSessionStatus::Running => "running",
        BackgroundSessionStatus::Completed => "completed",
        BackgroundSessionStatus::Failed => "failed",
        BackgroundSessionStatus::Cancelled => "cancelled",
        BackgroundSessionStatus::Delivered => "delivered",
    }
}

fn trace_background_tool(
    event_type: &str,
    background_task_id: &SubagentSessionId,
    agent_run_id: &AgentRunId,
    status: BackgroundSessionStatus,
    exit_code: Option<i64>,
) {
    tracing::debug!(
        target: "eos_engine::diagnostics",
        event_type,
        background_task_id = background_task_id.as_str(),
        task_kind = "subagent",
        tool_name = "run_subagent",
        agent_run_id = agent_run_id.as_str(),
        status = status_value(status),
        exit_code,
        "background tool lifecycle"
    );
}

pub(super) fn classify_run(run: AgentRunResult) -> (BackgroundSessionStatus, ToolResult, i64) {
    match run.terminal_result {
        Some(terminal) => {
            let exit_code = i64::from(terminal.is_error);
            let mut metadata = terminal.metadata.clone();
            metadata.insert("subagent_terminal_called".to_owned(), json!(true));
            let result = ToolResult {
                output: terminal.output,
                is_error: terminal.is_error,
                metadata,
                is_terminal: terminal.is_terminal,
            };
            (BackgroundSessionStatus::Completed, result, exit_code)
        }
        None => {
            let message = match run.error {
                Some(error) => format!("subagent crashed: {error}"),
                None => "subagent exited without calling a terminal tool. \
                         Findings were not delivered."
                    .to_owned(),
            };
            let result = ToolResult::error(message).meta("subagent_terminal_called", json!(false));
            (BackgroundSessionStatus::Failed, result, 1)
        }
    }
}

#[cfg(test)]
fn terminal_called(result: Option<&ToolResult>) -> bool {
    result
        .and_then(|result| result.metadata.get("subagent_terminal_called"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
fn subagent_status_and_result(
    status: BackgroundSessionStatus,
    result: Option<&ToolResult>,
) -> (&'static str, String) {
    let metadata = result.map(|result| &result.metadata);
    if let Some(reason) = metadata
        .and_then(|m| m.get("subagent_termination_reason"))
        .and_then(Value::as_str)
    {
        return ("terminated", format!("[terminated: {reason}] "));
    }
    if metadata
        .and_then(|m| m.get("subagent_cancelled"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return ("cancelled", "[cancelled] ".to_owned());
    }
    let output = || {
        result
            .map(|result| result.output.clone())
            .unwrap_or_default()
    };
    match status {
        BackgroundSessionStatus::Running => ("running", String::new()),
        BackgroundSessionStatus::Completed | BackgroundSessionStatus::Delivered
            if terminal_called(result) =>
        {
            ("finished", output())
        }
        BackgroundSessionStatus::Cancelled => ("cancelled", "[cancelled] ".to_owned()),
        _ => ("failed", output()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use eos_agent_def::AgentRegistry;
    use eos_audit::NoopAuditSink;
    use eos_llm_client::{LlmClient, LlmRequest, LlmStream, ProviderError};
    use eos_sandbox_port::SandboxTransport;
    use eos_skills::SkillRegistry;
    use eos_state::{
        AgentRun, AgentRunStore, CoreError, Sealed as StateSealed, TaskId, UtcDateTime,
    };
    use eos_testkit::{test_tools_root, FakeTransport};
    use eos_tools::{SandboxToolService, SkillToolService, ToolConfigSet};

    use crate::background::session_managers::BackgroundSessionManager;
    use crate::NotificationService;
    use crate::{
        AgentRunControlFactory, BackgroundSessionFactory, EngineRunHandles,
        ForegroundExecutorFactory,
    };

    use super::*;

    #[derive(Debug)]
    struct NoopLlmClient;

    #[async_trait]
    impl LlmClient for NoopLlmClient {
        async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Debug, Default)]
    struct NoopAgentRunStore;

    impl StateSealed for NoopAgentRunStore {}

    #[async_trait]
    impl AgentRunStore for NoopAgentRunStore {
        async fn create_run(
            &self,
            agent_run_id: &AgentRunId,
            task_id: &TaskId,
            agent_name: &str,
            initial_messages: Option<&[JsonObject]>,
        ) -> Result<AgentRun, CoreError> {
            Ok(AgentRun {
                id: agent_run_id.clone(),
                task_id: task_id.clone(),
                initial_messages: initial_messages.map(<[_]>::to_vec),
                agent_name: agent_name.to_owned(),
                message_history: None,
                terminal_tool_result: None,
                token_count: 0,
                error: None,
                created_at: UtcDateTime::now(),
                finished_at: None,
            })
        }

        async fn finish_run(
            &self,
            _agent_run_id: &AgentRunId,
            _message_history: Option<&[JsonObject]>,
            _terminal_tool_result: Option<&JsonObject>,
            _token_count: i64,
            _error: Option<&str>,
        ) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get(&self, _agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }

        async fn get_for_task(&self, _task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
            Ok(None)
        }
    }

    fn handles() -> EngineRunHandles {
        let transport: Arc<dyn SandboxTransport> = Arc::new(FakeTransport);
        EngineRunHandles {
            agent_run_store: Arc::new(NoopAgentRunStore),
            llm_client: Arc::new(NoopLlmClient),
            event_source_factory: None,
            agent_registry: Arc::new(
                Vec::<eos_agent_def::AgentDefinition>::new()
                    .into_iter()
                    .collect::<AgentRegistry>(),
            ),
            tool_config: Arc::new(
                ToolConfigSet::load_from_dir(&test_tools_root()).expect("tool config"),
            ),
            sandbox_service: SandboxToolService::new(transport),
            root_submission: None,
            skill_service: SkillToolService::new(Arc::new(SkillRegistry::new())),
            tool_registry_extender: None,
            audit: Arc::new(NoopAuditSink),
            message_records: None,
            workspace_root: "/tmp".to_owned(),
        }
    }

    fn manager(notifier: &NotificationService) -> SubagentSessionManager {
        let handles = handles();
        let control_factory = AgentRunControlFactory::new(
            ForegroundExecutorFactory,
            BackgroundSessionFactory::new(
                handles.clone(),
                Arc::new(FakeTransport),
                Duration::from_secs(3600),
                Arc::new(std::sync::OnceLock::new()),
            ),
        );
        SubagentSessionManager::new(
            handles,
            control_factory,
            BackgroundNotificationEmitter::new(notifier.clone()),
        )
    }

    fn tool_input(agent_name: &str) -> JsonObject {
        let mut input = JsonObject::new();
        input.insert("agent_name".to_owned(), json!(agent_name));
        input
    }

    #[test]
    fn terminal_with_error_settles_completed_and_finished() {
        let (status, result, exit_code) = classify_run(AgentRunResult {
            terminal_result: Some(ToolResult::error("partial but delivered")),
            error: None,
        });
        assert_eq!(status, BackgroundSessionStatus::Completed);
        assert!(result.is_error);
        assert_eq!(exit_code, 1);
        assert_eq!(
            subagent_status_and_result(BackgroundSessionStatus::Completed, Some(&result)).0,
            "finished"
        );
    }

    #[test]
    fn no_terminal_settles_failed() {
        let (status, result, _) = classify_run(AgentRunResult {
            terminal_result: None,
            error: None,
        });
        assert_eq!(status, BackgroundSessionStatus::Failed);
        assert!(result.output.contains("without calling a terminal tool"));
    }

    #[tokio::test]
    async fn count_cancel_and_finish_notification_are_manager_owned() {
        let notifier = NotificationService::new();
        let manager = manager(&notifier);
        let running_id: SubagentSessionId = "subagent_1".parse().expect("subagent id");
        let driver_abort = tokio::spawn(std::future::pending::<()>()).abort_handle();
        manager
            .insert(SubagentSession::running(
                running_id.clone(),
                "run-sub-1".parse().expect("agent run id"),
                driver_abort,
                tool_input("explorer"),
            ))
            .await;

        assert_eq!(manager.count().await, 1);
        assert!(manager.cancel_one(&running_id, "not needed").await);
        assert_eq!(manager.count().await, 0);

        let done_id: SubagentSessionId = "subagent_2".parse().expect("subagent id");
        let driver_abort = tokio::spawn(std::future::pending::<()>()).abort_handle();
        manager
            .insert(SubagentSession::running(
                done_id.clone(),
                "run-sub-2".parse().expect("agent run id"),
                driver_abort,
                tool_input("explorer"),
            ))
            .await;
        let completion = manager
            .settle(
                &done_id,
                BackgroundSessionStatus::Completed,
                ToolResult::ok("findings"),
            )
            .await
            .expect("completion");
        manager.finish(completion).await;

        let notifications = notifier.drain().await;
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0]
            .message
            .contains("[BACKGROUND COMPLETED] subagent_session_id=subagent_2"));
        assert!(notifications[0].message.contains("findings"));
    }
}
