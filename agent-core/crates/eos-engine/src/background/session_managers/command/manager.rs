use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_port::{
    cancel_workspace_runs_by_caller_id, collect_command_completions, SandboxTransport,
};
use eos_types::{AgentRunId, CommandSessionId, SandboxId};
use serde_json::Value;
use tokio::sync::Mutex;

use super::super::{BackgroundSession, BackgroundSessionManager, BackgroundSessionStatus};
use super::session::CommandSession;
use crate::background::notification::{BackgroundCompletion, BackgroundNotificationEmitter};

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
    command_port: Arc<dyn SandboxTransport>,
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
        command_port: Arc<dyn SandboxTransport>,
        notification: BackgroundNotificationEmitter,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            agent_run_id,
            command_port,
            notification,
        }
    }

    pub(in crate::background) async fn register(
        &self,
        command_session_id: &CommandSessionId,
        sandbox_id: &SandboxId,
        _command: &str,
    ) {
        let session = CommandSession::running(command_session_id.clone(), sandbox_id.clone());
        self.sessions
            .lock()
            .await
            .entry(command_session_id.clone())
            .or_insert(session);
    }

    pub(in crate::background) async fn command_session_result(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Option<Value> {
        let guard = self.sessions.lock().await;
        let session = guard.get(command_session_id)?;
        if matches!(session.status(), BackgroundSessionStatus::Running) {
            return None;
        }
        session.result().cloned()
    }

    pub(in crate::background) async fn mark_command_session_reported(
        &self,
        command_session_id: &CommandSessionId,
        result: Value,
    ) {
        if let Some(session) = self.sessions.lock().await.get_mut(command_session_id) {
            session.mark_reported(result);
        }
    }

    pub(in crate::background) async fn command_session_already_reported(
        &self,
        command_session_id: &CommandSessionId,
    ) -> bool {
        self.sessions
            .lock()
            .await
            .get(command_session_id)
            .is_some_and(|session| matches!(session.status(), BackgroundSessionStatus::Delivered))
    }

    fn running_by_sandbox(sessions: &CommandSessions) -> Vec<(SandboxId, Vec<String>)> {
        let mut groups: BTreeMap<SandboxId, Vec<String>> = BTreeMap::new();
        for session in sessions.values() {
            if matches!(session.status(), BackgroundSessionStatus::Running) {
                groups
                    .entry(session.sandbox_id().clone())
                    .or_default()
                    .push(session.id().as_str().to_owned());
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

    async fn poll(&self) -> Vec<Self::Completion> {
        let groups = {
            let guard = self.sessions.lock().await;
            Self::running_by_sandbox(&guard)
        };
        let mut out = Vec::new();
        for (sandbox_id, ids) in groups {
            let Ok(completions) = collect_command_completions(
                &*self.command_port,
                &sandbox_id,
                self.agent_run_id.as_str(),
                &ids,
            )
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

    async fn finish(&self, completion: Self::Completion) {
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
            if let Err(err) = cancel_workspace_runs_by_caller_id(
                &*self.command_port,
                &sandbox,
                self.agent_run_id.as_str(),
            )
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use eos_sandbox_port::{DaemonOp, SandboxPortError};
    use eos_types::JsonObject;
    use serde_json::json;
    use tokio::time::{sleep, timeout};

    use super::super::CommandSessionMonitor;
    use super::*;
    use crate::background::session_managers::{BackgroundSessionManager, BackgroundSessionMonitor};
    use crate::notifications::NotificationService;

    #[derive(Debug, Default)]
    struct RecordingTransport {
        calls: StdMutex<Vec<(DaemonOp, JsonObject)>>,
        collect_responses: StdMutex<VecDeque<JsonObject>>,
    }

    impl RecordingTransport {
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
    impl SandboxTransport for RecordingTransport {
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
            transport,
            BackgroundNotificationEmitter::new(notifier.clone()),
        )
    }

    #[tokio::test]
    async fn monitor_polls_and_emits_into_own_notifier() {
        let transport = Arc::new(RecordingTransport::with_collect([completion(
            "cmd_1", "ok", "3 passed",
        )]));
        let notifier = NotificationService::new();
        let manager = manager("agent-a", &notifier, transport.clone());
        manager
            .register(
                &"cmd_1".parse().expect("command id"),
                &"sandbox-a".parse().expect("sandbox id"),
                "cargo test",
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
        assert!(
            manager
                .command_session_already_reported(&"cmd_1".parse().expect("command id"))
                .await
        );
    }

    #[tokio::test]
    async fn cancel_issues_one_per_caller_rpc() {
        let transport = Arc::new(RecordingTransport::default());
        let notifier = NotificationService::new();
        let manager = manager("agent-a", &notifier, transport.clone());
        for id in ["cmd_1", "cmd_2"] {
            manager
                .register(
                    &id.parse().expect("command id"),
                    &"sandbox-a".parse().expect("sandbox id"),
                    "cargo test",
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
