use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eos_agent_run::{AgentRunApi, AgentRunOutcome, AgentRunStatus};
use eos_tool_core::{Sealed, SubagentSessionPort};
#[cfg(test)]
use eos_state::AgentRun;
use eos_tools::ToolResult;
#[cfg(test)]
use eos_types::JsonObject;
use eos_types::{AgentRunId, SubagentSessionId};
#[cfg(test)]
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};

/// One tracked subagent run owned by an agent run's background session runtime.
#[derive(Debug, Clone)]
pub(in crate::background) struct SubagentSession {
    id: SubagentSessionId,
    agent_run_id: AgentRunId,
    agent_name: String,
    status: BackgroundSessionStatus,
    result: Option<ToolResult>,
}

impl SubagentSession {
    fn tracked(id: SubagentSessionId, agent_run_id: AgentRunId, agent_name: String) -> Self {
        Self {
            id,
            agent_run_id,
            agent_name,
            status: BackgroundSessionStatus::Running,
            result: None,
        }
    }

    fn agent_run_id(&self) -> &AgentRunId {
        &self.agent_run_id
    }

    fn agent_name(&self) -> &str {
        &self.agent_name
    }

    const fn status(&self) -> BackgroundSessionStatus {
        self.status
    }

    fn result(&self) -> Option<&ToolResult> {
        self.result.as_ref()
    }

    fn cancel(&mut self, reason: &str) -> bool {
        if !matches!(self.status, BackgroundSessionStatus::Running) {
            return false;
        }
        self.status = BackgroundSessionStatus::Cancelled;
        self.result = Some(
            ToolResult::error(format!("Background subagent cancelled: {reason}"))
                .meta("subagent_cancelled", serde_json::json!(true)),
        );
        true
    }

    fn settle(
        &mut self,
        status: BackgroundSessionStatus,
        result: ToolResult,
    ) -> Option<ToolResult> {
        if status.precedence() > self.status.precedence() {
            self.status = status;
            self.result = Some(result);
        }
        self.result.clone()
    }
}

impl BackgroundSession for SubagentSession {
    type Id = SubagentSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}

#[derive(Debug, Clone)]
pub(in crate::background) struct SubagentCompletion {
    pub(super) subagent_session_id: SubagentSessionId,
    pub(super) status: BackgroundSessionStatus,
    pub(super) result: ToolResult,
}

#[derive(Default)]
struct SubagentSessionState {
    next_session_seq: u64,
    sessions: HashMap<SubagentSessionId, SubagentSession>,
}

/// Tracks subagent background sessions for one agent run.
#[derive(Clone)]
pub(in crate::background) struct SubagentSessionManager {
    agent_run_id: AgentRunId,
    sessions: Arc<Mutex<SubagentSessionState>>,
    agent_run_service: Arc<dyn AgentRunApi>,
    notification: BackgroundNotificationEmitter,
}

impl std::fmt::Debug for SubagentSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentSessionManager")
            .field("agent_run_id", &self.agent_run_id)
            .finish_non_exhaustive()
    }
}

impl SubagentSessionManager {
    pub(in crate::background) fn new(
        agent_run_id: AgentRunId,
        agent_run_service: Arc<dyn AgentRunApi>,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            agent_run_id,
            sessions: Arc::new(Mutex::new(SubagentSessionState::default())),
            agent_run_service,
            notification,
        }
    }

    pub(in crate::background) async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<(BackgroundSessionStatus, Option<ToolResult>, String)> {
        let guard = self.sessions.lock().await;
        let session = guard.sessions.get(subagent_session_id)?;
        let agent_name = session.agent_name().to_owned();
        Some((session.status(), session.result().cloned(), agent_name))
    }

    pub(in crate::background) async fn cancel_one(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> bool {
        let agent_run_id = {
            let mut guard = self.sessions.lock().await;
            let Some(session) = guard.sessions.get_mut(subagent_session_id) else {
                return false;
            };
            if session.cancel(reason) {
                Some(session.agent_run_id().clone())
            } else {
                None
            }
        };
        let Some(agent_run_id) = agent_run_id else {
            return false;
        };
        if let Err(err) = self
            .agent_run_service
            .cancel_agent_run(&agent_run_id, reason)
            .await
        {
            tracing::warn!(
                error = %err,
                agent_run_id = agent_run_id.as_str(),
                "background subagent cancellation failed"
            );
        }
        true
    }

    pub(in crate::background) async fn cancel_agent_run(
        &self,
        child_run_id: &AgentRunId,
        reason: &str,
    ) -> bool {
        let subagent_session_id = {
            let guard = self.sessions.lock().await;
            guard
                .sessions
                .values()
                .find(|session| session.agent_run_id() == child_run_id)
                .map(|session| session.id().clone())
        };
        let Some(subagent_session_id) = subagent_session_id else {
            return false;
        };
        self.cancel_one(&subagent_session_id, reason).await
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

    pub(in crate::background) async fn poll_completions(&self) -> Vec<SubagentCompletion> {
        let running = self.running_agent_runs().await;
        let mut completions = Vec::new();
        for (subagent_session_id, agent_run_id) in running {
            let terminal = match self
                .agent_run_service
                .poll_agent_run_outcome(&agent_run_id)
                .await
            {
                Ok(terminal) => terminal,
                Err(_) => continue,
            };
            let Some(terminal) = terminal else {
                continue;
            };
            let status = agent_run_status_to_background(terminal.status);
            let result = terminal_result(terminal);
            let is_error = result.is_error;
            if let Some(completion) = self.settle(&subagent_session_id, status, result).await {
                trace_background_tool(
                    terminal_event_type(status),
                    &subagent_session_id,
                    &agent_run_id,
                    status,
                    Some(i64::from(is_error)),
                );
                completions.push(completion);
            }
        }
        completions
    }

    async fn running_agent_runs(&self) -> Vec<(SubagentSessionId, AgentRunId)> {
        self.sessions
            .lock()
            .await
            .sessions
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .map(|session| (session.id().clone(), session.agent_run_id().clone()))
            .collect()
    }
}

pub(in crate::background) struct SubagentSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for SubagentSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl SubagentSessionMonitor {
    pub(in crate::background) fn spawn(
        manager: SubagentSessionManager,
        interval: Duration,
    ) -> Self {
        Self {
            join: tokio::spawn(async move {
                loop {
                    for completion in manager.poll_completions().await {
                        manager.push_notification_on_completion(completion).await;
                    }
                    tokio::time::sleep(interval).await;
                }
            }),
        }
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

    async fn push_notification_on_completion(&self, completion: Self::Completion) {
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
        let actions = {
            let mut guard = self.sessions.lock().await;
            guard
                .sessions
                .values_mut()
                .filter_map(|session| {
                    if session.cancel(reason) {
                        Some(session.agent_run_id().clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        for agent_run_id in actions {
            if let Err(err) = self
                .agent_run_service
                .cancel_agent_run(&agent_run_id, reason)
                .await
            {
                tracing::warn!(
                    error = %err,
                    agent_run_id = agent_run_id.as_str(),
                    "background subagent cancellation failed"
                );
            }
        }
    }
}

impl Sealed for SubagentSessionManager {}

#[async_trait]
impl SubagentSessionPort for SubagentSessionManager {
    async fn register_background_session(
        &self,
        agent_run_id: &AgentRunId,
        agent_name: &str,
    ) -> SubagentSessionId {
        let subagent_session_id = self.next_session_id().await;
        trace_background_tool(
            "background_tool.started",
            &subagent_session_id,
            agent_run_id,
            BackgroundSessionStatus::Running,
            None,
        );
        self.insert(SubagentSession::tracked(
            subagent_session_id.clone(),
            agent_run_id.clone(),
            agent_name.to_owned(),
        ))
        .await;
        subagent_session_id
    }

    async fn subagent_session_snapshot(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<eos_tool_core::SubagentProgress> {
        self.progress(subagent_session_id)
            .await
            .map(
                |(status, result, agent_name)| eos_tool_core::SubagentProgress::Found {
                    subagent_session_id: subagent_session_id.clone(),
                    status: background_status_to_subagent(status),
                    agent_name,
                    result,
                },
            )
    }

    async fn cancel_background_session(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> eos_tool_core::CancelledSubagent {
        if self.cancel_one(subagent_session_id, reason).await {
            eos_tool_core::CancelledSubagent::Cancelled {
                subagent_session_id: subagent_session_id.clone(),
                reason: reason.to_owned(),
            }
        } else {
            eos_tool_core::CancelledSubagent::MissingOrSettled {
                subagent_session_id: subagent_session_id.clone(),
            }
        }
    }

    async fn cancel_background_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> bool {
        self.cancel_agent_run(agent_run_id, reason).await
    }

    async fn count_background_sessions(&self) -> usize {
        BackgroundSessionManager::count(self).await
    }

    async fn cancel_all_background_sessions(&self, reason: &str) {
        BackgroundSessionManager::cancel(self, reason).await;
    }

    async fn poll_complete_background_sessions(&self) -> usize {
        let completions = self.poll_completions().await;
        let count = completions.len();
        for completion in completions {
            self.push_notification_on_completion(completion).await;
        }
        count
    }
}

fn terminal_result(outcome: AgentRunOutcome) -> ToolResult {
    outcome.terminal_result.unwrap_or_else(|| {
        ToolResult::error(
            outcome
                .error
                .unwrap_or_else(|| "subagent exited without terminal output".to_owned()),
        )
        .meta("subagent_terminal_called", serde_json::json!(false))
    })
}

const fn agent_run_status_to_background(status: AgentRunStatus) -> BackgroundSessionStatus {
    match status {
        AgentRunStatus::Completed => BackgroundSessionStatus::Completed,
        AgentRunStatus::Failed => BackgroundSessionStatus::Failed,
        AgentRunStatus::Cancelled => BackgroundSessionStatus::Cancelled,
    }
}

const fn background_status_to_subagent(
    status: BackgroundSessionStatus,
) -> eos_tool_core::SubagentSessionStatus {
    match status {
        BackgroundSessionStatus::Running => eos_tool_core::SubagentSessionStatus::Running,
        BackgroundSessionStatus::Completed => eos_tool_core::SubagentSessionStatus::Completed,
        BackgroundSessionStatus::Failed => eos_tool_core::SubagentSessionStatus::Failed,
        BackgroundSessionStatus::Cancelled => eos_tool_core::SubagentSessionStatus::Cancelled,
        BackgroundSessionStatus::Delivered => eos_tool_core::SubagentSessionStatus::Delivered,
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

#[cfg(test)]
pub(super) fn completion_from_agent_run(
    run: &AgentRun,
) -> Option<(BackgroundSessionStatus, ToolResult, i64)> {
    run.finished_at?;
    if let Some(terminal) = &run.terminal_tool_result {
        let result = tool_result_from_payload(terminal);
        let exit_code = i64::from(result.is_error);
        return Some((BackgroundSessionStatus::Completed, result, exit_code));
    }
    let message = match &run.error {
        Some(error) => format!("subagent crashed: {error}"),
        None => "subagent exited without calling a terminal tool. Findings were not delivered."
            .to_owned(),
    };
    Some((
        BackgroundSessionStatus::Failed,
        ToolResult::error(message).meta("subagent_terminal_called", json!(false)),
        1,
    ))
}

#[cfg(test)]
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

    use async_trait::async_trait;
    use eos_agent_run::{AgentRunError, SpawnAgentRequest};
    use eos_state::{AgentRun, UtcDateTime};

    use crate::NotificationService;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeAgentRunService;

    #[async_trait]
    impl AgentRunApi for FakeAgentRunService {
        async fn spawn_agent(
            &self,
            _request: SpawnAgentRequest,
        ) -> Result<AgentRunId, AgentRunError> {
            Ok(AgentRunId::new_v4())
        }

        async fn wait_for_agent_outcomes(
            &self,
            agent_run_id: &AgentRunId,
        ) -> Result<AgentRunOutcome, AgentRunError> {
            Err(AgentRunError::NotActiveInProcess(agent_run_id.clone()))
        }

        async fn poll_agent_run_outcome(
            &self,
            _agent_run_id: &AgentRunId,
        ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
            Ok(None)
        }

        async fn cancel_agent_run(
            &self,
            _agent_run_id: &AgentRunId,
            _reason: &str,
        ) -> Result<(), AgentRunError> {
            Ok(())
        }
    }

    fn manager(notifier: &NotificationService) -> SubagentSessionManager {
        SubagentSessionManager::new(
            "owner-run".parse().expect("agent run id"),
            Arc::new(FakeAgentRunService),
            BackgroundNotificationEmitter::new(notifier.clone()),
        )
    }

    fn finished_run(terminal_tool_result: Option<JsonObject>, error: Option<&str>) -> AgentRun {
        AgentRun {
            id: "run-sub-finished".parse().expect("agent run id"),
            task_id: None,
            initial_messages: None,
            agent_name: "explorer".to_owned(),
            message_history: None,
            terminal_tool_result,
            token_count: 0,
            error: error.map(str::to_owned),
            created_at: UtcDateTime::now(),
            finished_at: Some(UtcDateTime::now()),
        }
    }

    #[test]
    fn terminal_payload_settles_completed_and_finished() {
        let mut terminal = JsonObject::new();
        terminal.insert("output".to_owned(), json!("partial but delivered"));
        terminal.insert("is_error".to_owned(), json!(true));
        terminal.insert("metadata".to_owned(), json!({}));
        terminal.insert("is_terminal".to_owned(), json!(true));
        let (status, result, exit_code) =
            completion_from_agent_run(&finished_run(Some(terminal), None)).expect("completion");
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
        let (status, result, _) =
            completion_from_agent_run(&finished_run(None, None)).expect("completion");
        assert_eq!(status, BackgroundSessionStatus::Failed);
        assert!(result.output.contains("without calling a terminal tool"));
    }

    #[tokio::test]
    async fn count_cancel_and_completion_notification_are_manager_owned() {
        let notifier = NotificationService::new();
        let manager = manager(&notifier);
        let running_id: SubagentSessionId = "subagent_1".parse().expect("subagent id");
        manager
            .insert(SubagentSession::tracked(
                running_id.clone(),
                "run-sub-1".parse().expect("agent run id"),
                "explorer".to_owned(),
            ))
            .await;

        assert_eq!(manager.count().await, 1);
        assert!(manager.cancel_one(&running_id, "not needed").await);
        assert_eq!(manager.count().await, 0);

        let done_id: SubagentSessionId = "subagent_2".parse().expect("subagent id");
        manager
            .insert(SubagentSession::tracked(
                done_id.clone(),
                "run-sub-2".parse().expect("agent run id"),
                "explorer".to_owned(),
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
        manager.push_notification_on_completion(completion).await;

        let notifications = notifier.drain().await;
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0]
            .message
            .contains("[BACKGROUND COMPLETED] subagent_session_id=subagent_2"));
        assert!(notifications[0].message.contains("findings"));
    }
}
