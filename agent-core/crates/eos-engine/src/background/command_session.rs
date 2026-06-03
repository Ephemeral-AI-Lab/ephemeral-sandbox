//! Background PTY command-session supervision (anchor §5): the per-session
//! record, the [`BackgroundTaskSupervisor`] methods that ingest daemon
//! completions and render `[BACKGROUND COMPLETED]` notifications, and the
//! [`CommandSessionSupervisorPort`] implementation the `exec_command` /
//! `write_stdin` tools call through.
//!
//! The supervisor never touches the daemon; the heartbeat
//! ([`super::heartbeat`]) is the sole completion-pull driver. The `result`
//! payloads are the daemon completion's `result` map (status, `exit_code`,
//! `output.stdout`, …) — opaque JSON the supervisor only renders.

use async_trait::async_trait;
use eos_tools::ports::CommandSessionSupervisorPort;
use eos_tools::SystemNotification as ToolNotification;
use serde_json::Value;

use super::supervisor::{
    BackgroundSupervisorHandle, BackgroundTaskStatus, BackgroundTaskSupervisor,
};

/// One tracked background command session. `status` reuses
/// [`BackgroundTaskStatus`] (`Running` → `Completed`/`Failed`/`Cancelled` →
/// `Delivered`); `result` holds the terminal completion payload once known.
#[derive(Debug, Clone)]
pub struct CommandSessionRecord {
    /// Daemon-minted `cmd_<n>` — the correlation key.
    pub command_session_id: String,
    /// Owning sandbox id.
    pub sandbox_id: String,
    /// Owning agent id (`agent_run_id`) — per-task-run ownership.
    pub agent_id: String,
    /// The launched command, for the notification body.
    pub command: String,
    /// Lifecycle status.
    pub status: BackgroundTaskStatus,
    /// Terminal completion payload (`None` until terminal).
    pub result: Option<Value>,
}

/// Map a daemon completion `result.status` to a terminal supervisor status:
/// `ok` → `Completed`, `cancelled` → `Cancelled`, anything else
/// (`error`/`timed_out`) → `Failed`.
fn command_completion_status(result: Option<&Value>) -> BackgroundTaskStatus {
    match result
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
    {
        Some("ok") => BackgroundTaskStatus::Completed,
        Some("cancelled") => BackgroundTaskStatus::Cancelled,
        _ => BackgroundTaskStatus::Failed,
    }
}

/// Render the `[BACKGROUND COMPLETED]` notification body for a terminal record.
fn render_command_completion(record: &CommandSessionRecord) -> String {
    let result = record.result.as_ref();
    let status = result
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let exit_code = result
        .and_then(|result| result.get("exit_code"))
        .and_then(Value::as_i64);
    let stdout = result
        .and_then(|result| {
            result
                .get("output")
                .and_then(|output| output.get("stdout"))
                .or_else(|| result.get("stdout"))
        })
        .and_then(Value::as_str)
        .unwrap_or("");
    let exit = exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string());
    format!(
        "[BACKGROUND COMPLETED] command_session_id={} status={status} exit_code={exit}\n\
         command: {}\nstdout: {stdout}",
        record.command_session_id, record.command,
    )
}

impl BackgroundTaskSupervisor {
    /// Register a freshly-started background command session as running.
    /// Idempotent: an existing record (already terminal or running) is kept.
    pub fn register_command_session(
        &mut self,
        command_session_id: &str,
        sandbox_id: &str,
        agent_id: &str,
        command: &str,
    ) {
        self.command_sessions
            .entry(command_session_id.to_owned())
            .or_insert_with(|| CommandSessionRecord {
                command_session_id: command_session_id.to_owned(),
                sandbox_id: sandbox_id.to_owned(),
                agent_id: agent_id.to_owned(),
                command: command.to_owned(),
                status: BackgroundTaskStatus::Running,
                result: None,
            });
    }

    /// Apply one pulled daemon completion to its record (heartbeat path). Only a
    /// still-`Running` record is updated, so an already-reported terminal (the
    /// recover/mark latch) is never re-opened.
    pub fn ingest_completion(&mut self, completion: &Value) {
        let Some(id) = completion.get("command_session_id").and_then(Value::as_str) else {
            return;
        };
        let Some(record) = self.command_sessions.get_mut(id) else {
            return;
        };
        if !matches!(record.status, BackgroundTaskStatus::Running) {
            return;
        }
        let result = completion.get("result").cloned();
        record.status = command_completion_status(result.as_ref());
        record.result = result;
    }

    /// Render one `[BACKGROUND COMPLETED]` notification per terminal-undelivered
    /// record and latch it to `Delivered` (exactly-once).
    pub fn drain_command_session_notifications(&mut self) -> Vec<ToolNotification> {
        let mut notifications = Vec::new();
        for record in self.command_sessions.values_mut() {
            let terminal = matches!(
                record.status,
                BackgroundTaskStatus::Completed
                    | BackgroundTaskStatus::Failed
                    | BackgroundTaskStatus::Cancelled
            );
            if terminal && record.result.is_some() {
                notifications.push(ToolNotification {
                    event: record.command_session_id.clone(),
                    message: render_command_completion(record),
                });
                record.status = BackgroundTaskStatus::Delivered;
            }
        }
        notifications
    }

    /// The stored terminal result for a session that is no longer running (the
    /// recover race), else `None`.
    #[must_use]
    pub fn command_session_result(&self, command_session_id: &str) -> Option<Value> {
        let record = self.command_sessions.get(command_session_id)?;
        if matches!(record.status, BackgroundTaskStatus::Running) {
            return None;
        }
        record.result.clone()
    }

    /// Latch a session to `Delivered` with the terminal `result` a control tool
    /// observed inline, so the heartbeat does not re-deliver it.
    pub fn mark_command_session_reported(&mut self, command_session_id: &str, result: Value) {
        if let Some(record) = self.command_sessions.get_mut(command_session_id) {
            record.status = BackgroundTaskStatus::Delivered;
            record.result = Some(result);
        }
    }

    /// Whether a tracked session's completion was already delivered to the model
    /// (the heartbeat latched it `Delivered`), so a late `write_stdin` poll can
    /// answer with a terse already-reported note instead of the full payload.
    #[must_use]
    pub fn command_session_already_reported(&self, command_session_id: &str) -> bool {
        self.command_sessions
            .get(command_session_id)
            .is_some_and(|record| matches!(record.status, BackgroundTaskStatus::Delivered))
    }

    /// Running command-session ids grouped by `(sandbox_id, agent_id)` — the
    /// heartbeat's pull plan (deterministic order for stable polling).
    #[must_use]
    pub fn running_command_session_ids_by_sandbox_agent(
        &self,
    ) -> Vec<((String, String), Vec<String>)> {
        let mut groups: std::collections::BTreeMap<(String, String), Vec<String>> =
            std::collections::BTreeMap::new();
        for record in self.command_sessions.values() {
            if matches!(record.status, BackgroundTaskStatus::Running) {
                groups
                    .entry((record.sandbox_id.clone(), record.agent_id.clone()))
                    .or_default()
                    .push(record.command_session_id.clone());
            }
        }
        groups.into_iter().collect()
    }

    /// Count this agent's tracked, still-running command sessions (empty
    /// `agent_id` counts all).
    #[must_use]
    pub fn count_command_sessions_by_agent(&self, agent_id: &str) -> usize {
        self.command_sessions
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id == agent_id)
            })
            .count()
    }
}

#[async_trait]
impl CommandSessionSupervisorPort for BackgroundSupervisorHandle {
    async fn register(
        &self,
        command_session_id: &str,
        sandbox_id: &str,
        agent_id: &str,
        command: &str,
    ) {
        self.inner().lock().await.register_command_session(
            command_session_id,
            sandbox_id,
            agent_id,
            command,
        );
    }

    async fn command_session_result(&self, command_session_id: &str) -> Option<Value> {
        self.inner()
            .lock()
            .await
            .command_session_result(command_session_id)
    }

    async fn mark_command_session_reported(&self, command_session_id: &str, result: Value) {
        self.inner()
            .lock()
            .await
            .mark_command_session_reported(command_session_id, result);
    }

    async fn command_session_already_reported(&self, command_session_id: &str) -> bool {
        self.inner()
            .lock()
            .await
            .command_session_already_reported(command_session_id)
    }

    async fn count_by_agent(&self, agent_id: &str) -> usize {
        self.inner()
            .lock()
            .await
            .count_command_sessions_by_agent(agent_id)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn completion(id: &str, agent: &str, status: &str, stdout: &str) -> Value {
        json!({
            "command_session_id": id,
            "agent_id": agent,
            "command": "pytest -q",
            "result": {
                "status": status,
                "exit_code": if status == "ok" { 0 } else { 1 },
                "output": {"stdout": stdout, "stderr": ""},
            },
        })
    }

    #[test]
    fn register_pull_completed_flips_count_and_renders_once() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        supervisor.register_command_session("cmd_1", "sb", "agent-a", "pytest -q");
        assert_eq!(supervisor.count_command_sessions_by_agent("agent-a"), 1);

        supervisor.ingest_completion(&completion("cmd_1", "agent-a", "ok", "3 passed"));
        // Terminal → no longer counted as running.
        assert_eq!(supervisor.count_command_sessions_by_agent("agent-a"), 0);

        let first = supervisor.drain_command_session_notifications();
        assert_eq!(first.len(), 1);
        assert!(first[0].message.contains("[BACKGROUND COMPLETED]"));
        assert!(first[0].message.contains("command_session_id=cmd_1"));
        assert!(first[0].message.contains("status=ok"));
        assert!(first[0].message.contains("3 passed"));

        // Exactly-once: the Delivered latch suppresses a second drain.
        assert!(supervisor.drain_command_session_notifications().is_empty());
    }

    #[test]
    fn recover_race_returns_stored_terminal_and_marks_reported() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        supervisor.register_command_session("cmd_2", "sb", "agent-a", "make");
        // Still running → no recoverable result yet, not reported.
        assert!(supervisor.command_session_result("cmd_2").is_none());
        assert!(!supervisor.command_session_already_reported("cmd_2"));

        supervisor.ingest_completion(&completion("cmd_2", "agent-a", "error", "boom"));
        // Terminal (Failed) → recover returns the stored result; not yet reported.
        let recovered = supervisor
            .command_session_result("cmd_2")
            .expect("stored terminal");
        assert_eq!(recovered["status"], "error");
        assert!(!supervisor.command_session_already_reported("cmd_2"));

        // The control tool latches it Delivered → heartbeat drain stays empty and
        // a late write_stdin poll sees it already-reported (the terse §8/D8 path).
        supervisor.mark_command_session_reported("cmd_2", recovered);
        assert!(supervisor.drain_command_session_notifications().is_empty());
        assert!(supervisor.command_session_already_reported("cmd_2"));
    }

    #[test]
    fn running_ids_group_by_sandbox_and_agent() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        supervisor.register_command_session("cmd_a", "sb1", "agent-a", "a");
        supervisor.register_command_session("cmd_b", "sb1", "agent-a", "b");
        supervisor.register_command_session("cmd_c", "sb2", "agent-b", "c");
        let groups = supervisor.running_command_session_ids_by_sandbox_agent();
        assert_eq!(groups.len(), 2);
        let agent_a = groups
            .iter()
            .find(|((sandbox, agent), _)| sandbox == "sb1" && agent == "agent-a")
            .expect("agent-a group");
        assert_eq!(agent_a.1.len(), 2);
    }

    #[test]
    fn untracked_completion_is_ignored() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        supervisor.ingest_completion(&completion("cmd_unknown", "agent-a", "ok", "x"));
        assert!(supervisor.drain_command_session_notifications().is_empty());
    }
}
