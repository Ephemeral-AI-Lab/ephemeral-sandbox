use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eos_sandbox_port::SandboxCommandApi;
use eos_tool_core::{CommandSessionPort, Sealed};
use eos_types::{AgentRunId, CommandSessionId, SandboxId};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};

/// One tracked background command session.
#[derive(Debug, Clone)]
pub(in crate::background) struct CommandSession {
    id: CommandSessionId,
    sandbox_id: SandboxId,
    status: BackgroundSessionStatus,
    result: Option<Value>,
}

impl CommandSession {
    fn running(id: CommandSessionId, sandbox_id: SandboxId) -> Self {
        Self {
            id,
            sandbox_id,
            status: BackgroundSessionStatus::Running,
            result: None,
        }
    }

    fn sandbox_id(&self) -> &SandboxId {
        &self.sandbox_id
    }

    const fn status(&self) -> BackgroundSessionStatus {
        self.status
    }

    fn deliver(&mut self, result: Value) -> BackgroundSessionStatus {
        let status = command_completion_status(Some(&result));
        self.result = Some(result);
        self.status = BackgroundSessionStatus::Delivered;
        status
    }

    fn cancel(&mut self) {
        if matches!(self.status, BackgroundSessionStatus::Running) {
            self.status = BackgroundSessionStatus::Cancelled;
            self.result = Some(serde_json::json!({
                "status": "cancelled",
                "exit_code": Value::Null,
                "output": {"stdout": "", "stderr": ""},
            }));
        }
    }
}

impl BackgroundSession for CommandSession {
    type Id = CommandSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}

fn command_completion_status(result: Option<&Value>) -> BackgroundSessionStatus {
    match result
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
    {
        Some("ok") => BackgroundSessionStatus::Completed,
        Some("cancelled") => BackgroundSessionStatus::Cancelled,
        _ => BackgroundSessionStatus::Failed,
    }
}

type CommandSessions = HashMap<CommandSessionId, CommandSession>;

#[derive(Debug, Clone)]
pub(in crate::background) struct CommandCompletion {
    pub(super) command_session_id: CommandSessionId,
    pub(super) sandbox_id: SandboxId,
    pub(super) status: BackgroundSessionStatus,
    pub(super) result: Value,
}

/// Tracks sandbox command sessions for one agent run.
#[derive(Clone)]
pub(in crate::background) struct CommandSessionManager {
    sessions: Arc<Mutex<CommandSessions>>,
    agent_run_id: AgentRunId,
    command_service: Arc<dyn SandboxCommandApi>,
    notification: BackgroundNotificationEmitter,
}

impl std::fmt::Debug for CommandSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSessionManager")
            .field("agent_run_id", &self.agent_run_id)
            .finish_non_exhaustive()
    }
}

impl CommandSessionManager {
    pub(in crate::background) fn new(
        agent_run_id: AgentRunId,
        command_service: Arc<dyn SandboxCommandApi>,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            agent_run_id,
            command_service,
            notification,
        }
    }

    pub(in crate::background) async fn register_background_session(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
    ) {
        let session = CommandSession::running(command_session_id.clone(), sandbox_id.clone());
        self.sessions
            .lock()
            .await
            .entry(command_session_id.clone())
            .or_insert(session);
    }

    fn running_by_sandbox(sessions: &CommandSessions) -> Vec<(SandboxId, Vec<CommandSessionId>)> {
        let mut groups: BTreeMap<SandboxId, Vec<CommandSessionId>> = BTreeMap::new();
        for session in sessions.values() {
            if matches!(session.status(), BackgroundSessionStatus::Running) {
                groups
                    .entry(session.sandbox_id().clone())
                    .or_default()
                    .push(session.id().clone());
            }
        }
        groups.into_iter().collect()
    }

    fn running_sandboxes(sessions: &CommandSessions) -> Vec<SandboxId> {
        let mut seen: BTreeMap<SandboxId, ()> = BTreeMap::new();
        for session in sessions.values() {
            if matches!(session.status(), BackgroundSessionStatus::Running) {
                seen.insert(session.sandbox_id().clone(), ());
            }
        }
        seen.into_keys().collect()
    }

    fn ingest_completions(
        sessions: &mut CommandSessions,
        completions: &[eos_types::JsonObject],
    ) -> Vec<CommandCompletion> {
        let mut out = Vec::new();
        for completion in completions {
            let Some(id) = completion
                .get("command_session_id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<CommandSessionId>().ok())
            else {
                continue;
            };
            let Some(session) = sessions.get_mut(&id) else {
                continue;
            };
            if !matches!(session.status(), BackgroundSessionStatus::Running) {
                continue;
            }
            let result = completion.get("result").cloned().unwrap_or(Value::Null);
            let status = session.deliver(result.clone());
            out.push(CommandCompletion {
                command_session_id: id,
                sandbox_id: session.sandbox_id().clone(),
                status,
                result,
            });
        }
        out
    }

    pub(in crate::background) async fn poll_completions(&self) -> Vec<CommandCompletion> {
        let groups = {
            let guard = self.sessions.lock().await;
            Self::running_by_sandbox(&guard)
        };
        let mut out = Vec::new();
        for (sandbox_id, ids) in groups {
            let Ok(completions) = self
                .command_service
                .collect_completed_commands(&sandbox_id, &self.agent_run_id, &ids)
                .await
            else {
                continue;
            };
            if completions.is_empty() {
                continue;
            }
            let mut guard = self.sessions.lock().await;
            out.extend(Self::ingest_completions(&mut guard, &completions));
        }
        out
    }
}

pub(in crate::background) struct CommandSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for CommandSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl CommandSessionMonitor {
    pub(in crate::background) fn spawn(manager: CommandSessionManager, interval: Duration) -> Self {
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
impl BackgroundSessionManager for CommandSessionManager {
    type Session = CommandSession;
    type Completion = CommandCompletion;

    async fn insert(&self, session: Self::Session) {
        self.sessions
            .lock()
            .await
            .insert(session.id().clone(), session);
    }

    async fn count(&self) -> usize {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|session| matches!(session.status(), BackgroundSessionStatus::Running))
            .count()
    }

    async fn push_notification_on_completion(&self, completion: Self::Completion) {
        let _ = self
            .notification
            .emit(BackgroundCompletion::CommandSession {
                command_session_id: completion.command_session_id,
                sandbox_id: completion.sandbox_id,
                status: completion.status,
                result: completion.result,
            })
            .await;
    }

    async fn cancel(&self, reason: &str) {
        let sandboxes = {
            let guard = self.sessions.lock().await;
            Self::running_sandboxes(&guard)
        };
        for sandbox in sandboxes {
            if let Err(err) = self
                .command_service
                .cancel_commands_for_run(&sandbox, &self.agent_run_id, reason)
                .await
            {
                tracing::warn!(
                    error = %err,
                    caller_id = self.agent_run_id.as_str(),
                    sandbox_id = sandbox.as_str(),
                    reason,
                    "per-caller workspace-run cancellation failed"
                );
            }
        }
        for session in self.sessions.lock().await.values_mut() {
            session.cancel();
        }
    }
}

impl Sealed for CommandSessionManager {}

#[async_trait]
impl CommandSessionPort for CommandSessionManager {
    async fn register_background_session(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
    ) {
        CommandSessionManager::register_background_session(self, command_session_id, sandbox_id)
            .await;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use eos_sandbox_port::{DaemonOp, SandboxPortError, SandboxTransport};
    use eos_types::JsonObject;
    use serde_json::json;
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::notifications::NotificationService;
    use eos_sandbox_port::SandboxCommandService;

    #[derive(Debug, Default)]
    struct CommandSessionTestTransport {
        calls: StdMutex<Vec<(DaemonOp, JsonObject)>>,
        collect_responses: StdMutex<VecDeque<JsonObject>>,
    }

    impl CommandSessionTestTransport {
        fn with_collect(responses: impl IntoIterator<Item = JsonObject>) -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                collect_responses: StdMutex::new(responses.into_iter().collect()),
            }
        }

        fn payloads(&self, op: DaemonOp) -> Vec<JsonObject> {
            self.calls
                .lock()
                .expect("calls")
                .iter()
                .filter(|(call_op, _)| *call_op == op)
                .map(|(_, payload)| payload.clone())
                .collect()
        }
    }

    #[async_trait]
    impl SandboxTransport for CommandSessionTestTransport {
        async fn call(
            &self,
            _sandbox_id: &SandboxId,
            op: DaemonOp,
            payload: JsonObject,
            _timeout_s: u32,
        ) -> Result<JsonObject, SandboxPortError> {
            self.calls.lock().expect("calls").push((op, payload));
            let response = match op {
                DaemonOp::CommandCollectCompleted => self
                    .collect_responses
                    .lock()
                    .expect("responses")
                    .pop_front()
                    .unwrap_or_default(),
                _ => json!({"success": true})
                    .as_object()
                    .expect("object")
                    .clone(),
            };
            Ok(response)
        }
    }

    fn completion(id: &str, status: &str, stdout: &str) -> JsonObject {
        json!({
            "completions": [{
                "command_session_id": id,
                "result": {
                    "status": status,
                    "exit_code": if status == "ok" { 0 } else { 1 },
                    "output": {"stdout": stdout, "stderr": ""},
                },
            }]
        })
        .as_object()
        .expect("object")
        .clone()
    }

    fn manager(
        owner: &str,
        notifier: &NotificationService,
        transport: Arc<dyn SandboxTransport>,
    ) -> CommandSessionManager {
        CommandSessionManager::new(
            owner.parse().expect("agent run id"),
            Arc::new(SandboxCommandService::new(transport)),
            BackgroundNotificationEmitter::new(notifier.clone()),
        )
    }

    #[tokio::test]
    async fn monitor_polls_and_emits_into_own_notifier() {
        let transport = Arc::new(CommandSessionTestTransport::with_collect([completion(
            "cmd_1", "ok", "3 passed",
        )]));
        let notifier = NotificationService::new();
        let manager = manager("agent-a", &notifier, transport.clone());
        manager
            .register_background_session(
                &"cmd_1".parse().expect("command id"),
                &"sandbox-a".parse().expect("sandbox id"),
            )
            .await;
        let _monitor = CommandSessionMonitor::spawn(manager.clone(), Duration::from_millis(1));

        let notifications = timeout(Duration::from_millis(200), async {
            loop {
                let drained = notifier.drain().await;
                if !drained.is_empty() {
                    break drained;
                }
                sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("notification");

        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].message.contains("[BACKGROUND COMPLETED]"));
        assert!(notifications[0].message.contains("cmd_1"));
        assert!(notifications[0].message.contains("3 passed"));
        let collect = transport.payloads(DaemonOp::CommandCollectCompleted);
        assert!(!collect.is_empty());
        assert_eq!(collect[0]["caller_id"], json!("agent-a"));
        assert_eq!(manager.count().await, 0);
    }

    #[tokio::test]
    async fn cancel_issues_one_per_caller_rpc() {
        let transport = Arc::new(CommandSessionTestTransport::default());
        let notifier = NotificationService::new();
        let manager = manager("agent-a", &notifier, transport.clone());
        for id in ["cmd_1", "cmd_2"] {
            manager
                .register_background_session(
                    &id.parse().expect("command id"),
                    &"sandbox-a".parse().expect("sandbox id"),
                )
                .await;
        }
        manager.cancel("parent exited").await;
        let cancels = transport.payloads(DaemonOp::CancelWorkspaceRunsByCaller);
        assert_eq!(cancels.len(), 1, "one per-caller cancel for two sessions");
        assert_eq!(cancels[0]["caller_id"], json!("agent-a"));
        assert_eq!(manager.count().await, 0);
    }
}
