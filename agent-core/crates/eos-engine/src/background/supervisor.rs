//! Background task supervisor.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_tools::ports::{Sealed, StartedSubagent, SubagentSupervisorPort};
use eos_tools::{ToolError, ToolResult};
use eos_types::{JsonObject, SubagentSessionId};
use tokio::sync::Mutex;

/// Background task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskStatus {
    /// Task is running.
    Running,
    /// Task completed normally.
    Completed,
    /// Task failed.
    Failed,
    /// Task was cancelled.
    Cancelled,
    /// Result was delivered to the model.
    Delivered,
}

impl BackgroundTaskStatus {
    /// Terminal precedence; higher status wins when cancel/finish events race.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Cancelled => 1,
            Self::Failed => 2,
            Self::Completed => 3,
            Self::Delivered => 4,
        }
    }

    const fn is_terminal_undelivered(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Background task kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskKind {
    /// Local agent-run work.
    Agent,
    /// Subagent work.
    Subagent,
    /// Delegated workflow handle.
    Workflow,
}

/// How a task was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// Explicit cancel.
    Cancel,
    /// Early stop request.
    EarlyStop,
    /// Parent agent exited.
    ParentExit,
}

/// One background task record.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundTaskRecord {
    /// Supervisor-local id.
    pub task_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Original tool input.
    pub tool_input: JsonObject,
    /// Task kind.
    pub task_kind: BackgroundTaskKind,
    /// Current status.
    pub status: BackgroundTaskStatus,
    /// Cancellation reason.
    pub cancel_reason: Option<String>,
    /// Stop mode.
    pub stop_mode: Option<StopMode>,
    /// Final result.
    pub result: Option<ToolResult>,
    /// Progress lines.
    pub progress_lines: Vec<String>,
}

impl BackgroundTaskRecord {
    /// Whether the task has already been delivered to the model.
    #[must_use]
    pub const fn delivered(&self) -> bool {
        matches!(self.status, BackgroundTaskStatus::Delivered)
    }

    /// Whether the task still needs delivery.
    #[must_use]
    pub const fn outstanding(&self) -> bool {
        matches!(self.status, BackgroundTaskStatus::Running)
            || self.status.is_terminal_undelivered()
    }
}

/// Single-owner background supervisor state.
#[derive(Debug, Default)]
pub struct BackgroundTaskSupervisor {
    counter: u64,
    subagent_counter: u64,
    workflow_counter: u64,
    records: HashMap<String, BackgroundTaskRecord>,
}

impl BackgroundTaskSupervisor {
    /// Create an empty supervisor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a running task.
    pub fn register_running(
        &mut self,
        tool_name: &str,
        tool_input: JsonObject,
        task_kind: BackgroundTaskKind,
    ) -> String {
        let task_id = match task_kind {
            BackgroundTaskKind::Subagent => {
                self.subagent_counter = self.subagent_counter.saturating_add(1);
                format!("subagent_{}", self.subagent_counter)
            }
            BackgroundTaskKind::Workflow => {
                self.workflow_counter = self.workflow_counter.saturating_add(1);
                format!("wf_{}", self.workflow_counter)
            }
            BackgroundTaskKind::Agent => {
                self.counter = self.counter.saturating_add(1);
                format!("bg_{}", self.counter)
            }
        };
        self.records.insert(
            task_id.clone(),
            BackgroundTaskRecord {
                task_id: task_id.clone(),
                tool_name: tool_name.to_owned(),
                tool_input,
                task_kind,
                status: BackgroundTaskStatus::Running,
                cancel_reason: None,
                stop_mode: None,
                result: None,
                progress_lines: Vec::new(),
            },
        );
        task_id
    }

    /// Borrow a record.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<&BackgroundTaskRecord> {
        self.records.get(task_id)
    }

    /// Mark a task completed. Completion outranks a previously requested
    /// cancellation, preserving the cancel-vs-finish race behavior.
    pub fn complete(&mut self, task_id: &str, result: ToolResult) {
        if let Some(record) = self.records.get_mut(task_id) {
            let status = if result.is_error {
                BackgroundTaskStatus::Failed
            } else {
                BackgroundTaskStatus::Completed
            };
            if status.precedence() > record.status.precedence() {
                record.status = status;
                record.result = Some(result);
            }
        }
    }

    /// Request cancellation for a running task.
    pub fn cancel(&mut self, task_id: &str, reason: impl Into<String>) -> Option<String> {
        let reason = reason.into();
        let record = self.records.get_mut(task_id)?;
        if matches!(record.status, BackgroundTaskStatus::Running) {
            record.status = BackgroundTaskStatus::Cancelled;
            record.stop_mode = Some(StopMode::Cancel);
            record.cancel_reason = Some(reason.clone());
            record.result = Some(ToolResult::error(format!(
                "Background task cancelled: {reason}"
            )));
        }
        Some(format!("Cancelled background task `{task_id}`."))
    }

    /// Parent-exit cancellation for all still-running tasks.
    pub fn terminate_for_parent_exit(&mut self) {
        for record in self.records.values_mut() {
            if matches!(record.status, BackgroundTaskStatus::Running) {
                record.status = BackgroundTaskStatus::Cancelled;
                record.stop_mode = Some(StopMode::ParentExit);
                record.cancel_reason = Some("parent agent exited".to_owned());
                record.result = Some(ToolResult::error(
                    "Background task stopped because the parent agent exited.",
                ));
            }
        }
    }

    /// Count in-flight tasks.
    #[must_use]
    pub fn inflight_count(&self) -> usize {
        self.records
            .values()
            .filter(|record| matches!(record.status, BackgroundTaskStatus::Running))
            .count()
    }

    /// Append one progress line.
    pub fn push_progress(&mut self, task_id: &str, line: impl Into<String>) {
        if let Some(record) = self.records.get_mut(task_id) {
            record.progress_lines.push(line.into());
        }
    }
}

/// Shared port wrapper for `run_subagent`/progress/cancel tools.
#[derive(Debug, Clone, Default)]
pub struct SharedSubagentSupervisor {
    inner: Arc<Mutex<BackgroundTaskSupervisor>>,
}

impl SharedSubagentSupervisor {
    /// Create a shared wrapper around `supervisor`.
    #[must_use]
    pub fn new(supervisor: BackgroundTaskSupervisor) -> Self {
        Self {
            inner: Arc::new(Mutex::new(supervisor)),
        }
    }

    /// Access the shared supervisor for tests and runtime adapters.
    #[must_use]
    pub fn inner(&self) -> Arc<Mutex<BackgroundTaskSupervisor>> {
        self.inner.clone()
    }
}

impl Sealed for SharedSubagentSupervisor {}

#[async_trait]
impl SubagentSupervisorPort for SharedSubagentSupervisor {
    async fn spawn(&self, agent_name: &str, prompt: &str) -> Result<StartedSubagent, ToolError> {
        let mut input = JsonObject::new();
        input.insert("agent_name".to_owned(), serde_json::json!(agent_name));
        input.insert("prompt".to_owned(), serde_json::json!(prompt));
        let task_id = self.inner.lock().await.register_running(
            "run_subagent",
            input,
            BackgroundTaskKind::Subagent,
        );
        Ok(StartedSubagent {
            subagent_session_id: task_id.parse()?,
        })
    }

    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        last_n_messages: u8,
    ) -> Result<String, ToolError> {
        let supervisor = self.inner.lock().await;
        let Some(record) = supervisor.get(subagent_session_id.as_str()) else {
            return Ok(format!(
                "No subagent session `{}` is tracked.",
                subagent_session_id.as_str()
            ));
        };
        let len = record.progress_lines.len();
        let start = len.saturating_sub(usize::from(last_n_messages));
        let lines = record.progress_lines[start..].join("\n");
        Ok(format!("{:?}: {}", record.status, lines))
    }

    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<String, ToolError> {
        let mut supervisor = self.inner.lock().await;
        Ok(supervisor
            .cancel(subagent_session_id.as_str(), reason)
            .unwrap_or_else(|| {
                format!(
                    "No subagent session `{}` is tracked.",
                    subagent_session_id.as_str()
                )
            }))
    }

    async fn background_inflight_count(&self, _agent_id: &str) -> usize {
        self.inner.lock().await.inflight_count()
    }
}

#[cfg(test)]
mod tests {
    use eos_tools::ToolResult;

    use super::*;

    #[tokio::test]
    async fn parent_exit_and_cancel_complete_race() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let running = supervisor.register_running(
            "run_subagent",
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
        );
        supervisor.terminate_for_parent_exit();
        let record = supervisor.get(&running).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Cancelled);
        assert_eq!(record.stop_mode, Some(StopMode::ParentExit));
        assert!(record.outstanding());

        let racing = supervisor.register_running(
            "run_subagent",
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
        );
        supervisor.cancel(&racing, "no longer needed");
        supervisor.complete(&racing, ToolResult::ok("finished anyway"));
        let record = supervisor.get(&racing).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Completed);
        assert!(record.outstanding());
        assert_eq!(
            record.result.as_ref().map(|result| result.output.as_str()),
            Some("finished anyway")
        );

        let shared = SharedSubagentSupervisor::new(supervisor);
        assert_eq!(shared.background_inflight_count("agent").await, 0);
    }

    #[test]
    fn background_status_precedence_matches_source() {
        assert!(
            BackgroundTaskStatus::Completed.precedence()
                > BackgroundTaskStatus::Failed.precedence()
        );
        assert!(
            BackgroundTaskStatus::Failed.precedence()
                > BackgroundTaskStatus::Cancelled.precedence()
        );
        assert!(
            BackgroundTaskStatus::Delivered.precedence()
                > BackgroundTaskStatus::Completed.precedence()
        );
    }

    #[test]
    fn background_ids_use_typed_prefixes() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        assert_eq!(
            supervisor.register_running(
                "exec_command",
                JsonObject::new(),
                BackgroundTaskKind::Agent
            ),
            "bg_1"
        );
        assert_eq!(
            supervisor.register_running(
                "run_subagent",
                JsonObject::new(),
                BackgroundTaskKind::Subagent
            ),
            "subagent_1"
        );
        assert_eq!(
            supervisor.register_running(
                "delegate_workflow",
                JsonObject::new(),
                BackgroundTaskKind::Workflow
            ),
            "wf_1"
        );
    }
}
