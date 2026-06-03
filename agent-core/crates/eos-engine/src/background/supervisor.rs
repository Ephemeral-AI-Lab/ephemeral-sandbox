//! Background task supervisor — the single owner of every background kind:
//! subagent records (this module's `records`) + command sessions
//! (`command_session.rs`'s `command_sessions`). One ledger, one precedence
//! latch, one count surface ([`BackgroundInflightReport`]), one notifier.
//!
//! [`BackgroundSupervisorHandle`] wraps that state and is the real
//! [`SubagentSupervisorPort`](eos_tools::ports::SubagentSupervisorPort) (impl in
//! `subagent.rs`) and [`CommandSessionSupervisorPort`](eos_tools::ports::CommandSessionSupervisorPort)
//! (impl in `command_session.rs`). It also holds the [`EngineRunHandles`] +
//! [`AuditSink`] + [`Clock`] the subagent driver needs.

use std::collections::HashMap;
use std::sync::Arc;

use eos_audit::AuditSink;
use eos_tools::ports::Sealed;
use eos_tools::{BackgroundInflightReport, ToolResult};
use eos_types::{Clock, JsonObject};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::agent_loop::EngineRunHandles;

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

/// Background task kind. (`Agent`/`bg_<n>` was a dead, test-only alias minted
/// only by the deleted dispatch path; the production kinds are `Subagent` and
/// `Workflow`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskKind {
    /// Subagent work.
    Subagent,
    /// Delegated workflow handle.
    Workflow,
}

/// One background task record. `Debug, Clone, PartialEq` are preserved for the
/// tests; the non-cloneable [`AbortHandle`] rides in a side map on the
/// supervisor, not on the record.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundTaskRecord {
    /// Supervisor-local id.
    pub task_id: String,
    /// Original tool input.
    pub tool_input: JsonObject,
    /// Task kind.
    pub task_kind: BackgroundTaskKind,
    /// Current status.
    pub status: BackgroundTaskStatus,
    /// Owning agent id (the launching agent), for the agent-scoped count.
    pub agent_id: Option<String>,
    /// Final result.
    pub result: Option<ToolResult>,
}

impl BackgroundTaskRecord {
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
    subagent_counter: u64,
    workflow_counter: u64,
    records: HashMap<String, BackgroundTaskRecord>,
    /// Abort handles for running subagent task drivers, keyed by `task_id`.
    /// Resource hygiene only — what unwedges the terminal is the *settle* (the
    /// record leaves `Running`); `abort()` merely stops a runaway child.
    handles: HashMap<String, AbortHandle>,
    /// Tracked background PTY command sessions, keyed by daemon-minted
    /// `command_session_id` (anchor §5). Visible to the sibling
    /// `command_session` module that owns their lifecycle methods.
    pub(super) command_sessions: HashMap<String, super::command_session::CommandSessionRecord>,
}

impl BackgroundTaskSupervisor {
    /// Create an empty supervisor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a running task, stamping the owning `agent_id` (Python
    /// `BackgroundTaskRecord.agent_id`).
    pub fn register_running(
        &mut self,
        tool_input: JsonObject,
        task_kind: BackgroundTaskKind,
        agent_id: Option<String>,
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
        };
        self.records.insert(
            task_id.clone(),
            BackgroundTaskRecord {
                task_id: task_id.clone(),
                tool_input,
                task_kind,
                status: BackgroundTaskStatus::Running,
                agent_id,
                result: None,
            },
        );
        task_id
    }

    /// Borrow a record.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<&BackgroundTaskRecord> {
        self.records.get(task_id)
    }

    /// Settle a record to a terminal status with its result, gated by the
    /// precedence latch (Python `_done_callback` / `_apply_terminal_status_transition`):
    /// a higher-precedence outcome wins, so a finish racing a cancel resolves to
    /// `Completed`. This is the single on-completion routine for the subagent
    /// driver — the status is classified by terminal *presence* (Completed when a
    /// terminal was called, even if its `is_error` is true), not by `is_error`.
    pub fn settle(&mut self, task_id: &str, status: BackgroundTaskStatus, result: ToolResult) {
        if let Some(record) = self.records.get_mut(task_id) {
            if status.precedence() > record.status.precedence() {
                record.status = status;
                record.result = Some(result);
            }
        }
    }

    /// Cancel one tracked subagent, settling it `Cancelled`. Returns `false` for
    /// an unknown or already-settled session (Python `cancel_subagent_session`).
    pub fn cancel_subagent(&mut self, task_id: &str, reason: &str) -> bool {
        let Some(record) = self.records.get_mut(task_id) else {
            return false;
        };
        if !matches!(record.status, BackgroundTaskStatus::Running) {
            return false;
        }
        record.status = BackgroundTaskStatus::Cancelled;
        record.result = Some(
            ToolResult::error(format!("Background subagent cancelled: {reason}"))
                .meta("subagent_cancelled", json!(true)),
        );
        true
    }

    /// Parent-exit cancellation for all still-running tasks (+ abort their
    /// drivers).
    pub fn terminate_for_parent_exit(&mut self) {
        let ids: Vec<String> = self
            .records
            .values()
            .filter(|record| matches!(record.status, BackgroundTaskStatus::Running))
            .map(|record| record.task_id.clone())
            .collect();
        for id in ids {
            if let Some(record) = self.records.get_mut(&id) {
                record.status = BackgroundTaskStatus::Cancelled;
                record.result = Some(ToolResult::error(
                    "Background task stopped because the parent agent exited.",
                ));
            }
            self.take_and_abort_handle(&id);
        }
    }

    /// Drain this agent's in-flight subagent runs (settle `Cancelled` + abort the
    /// drivers), then return the post-drain report. The terminal/exit prehook
    /// runs this so a live or phantom subagent never wedges the agent's terminal
    /// (D9). Command sessions are intentionally not drained here (see the hook).
    pub fn drain_subagents_for_agent(&mut self, agent_id: &str) -> BackgroundInflightReport {
        let ids: Vec<String> = self
            .records
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches!(record.task_kind, BackgroundTaskKind::Subagent)
                    && (agent_id.is_empty() || record.agent_id.as_deref() == Some(agent_id))
            })
            .map(|record| record.task_id.clone())
            .collect();
        for id in ids {
            if let Some(record) = self.records.get_mut(&id) {
                record.status = BackgroundTaskStatus::Cancelled;
                record.result = Some(
                    ToolResult::error(
                        "Background subagent cancelled: parent submitted its terminal.",
                    )
                    .meta("subagent_cancelled", json!(true)),
                );
            }
            self.take_and_abort_handle(&id);
        }
        self.inflight_report(agent_id)
    }

    /// This agent's in-flight background report (Running-only): subagent records
    /// + supervisor-tracked command sessions. An empty `agent_id` counts all.
    #[must_use]
    pub fn inflight_report(&self, agent_id: &str) -> BackgroundInflightReport {
        let subagent = self
            .records
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches!(record.task_kind, BackgroundTaskKind::Subagent)
                    && (agent_id.is_empty() || record.agent_id.as_deref() == Some(agent_id))
            })
            .count();
        let command_session = self.count_command_sessions_by_agent(agent_id);
        // The supervisor does not own workflow lifecycle (sibling crate); the
        // terminal hook fills the workflow dimension from the authoritative
        // `WorkflowControlPort`, so the supervisor's own report leaves it 0.
        let workflow = 0;
        BackgroundInflightReport {
            total: subagent + workflow + command_session,
            subagent,
            workflow,
            command_session,
        }
    }

    /// Store a running subagent driver's abort handle.
    pub fn store_handle(&mut self, task_id: String, handle: AbortHandle) {
        self.handles.insert(task_id, handle);
    }

    /// Drop a finished driver's handle without aborting (the run already ended).
    pub fn forget_handle(&mut self, task_id: &str) {
        self.handles.remove(task_id);
    }

    /// Abort and drop a running driver's handle.
    pub fn take_and_abort_handle(&mut self, task_id: &str) {
        if let Some(handle) = self.handles.remove(task_id) {
            handle.abort();
        }
    }
}

/// The run dependencies the subagent driver needs, threaded in at the
/// composition root: the engine run handles (registry + stores + client + cwd),
/// the audit sink (single emitter of `background_tool.*`), and the clock.
#[derive(Clone)]
pub struct BackgroundSupervisorHandle {
    inner: Arc<Mutex<BackgroundTaskSupervisor>>,
    pub(super) handles: EngineRunHandles,
    pub(super) audit: Arc<dyn AuditSink>,
    pub(super) clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for BackgroundSupervisorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSupervisorHandle")
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisorHandle {
    /// Create the shared supervisor with the run handles + audit sink + clock the
    /// subagent driver needs. The ledger starts empty.
    #[must_use]
    pub fn new(
        handles: EngineRunHandles,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackgroundTaskSupervisor::new())),
            handles,
            audit,
            clock,
        }
    }

    /// Access the shared supervisor state for the heartbeat and runtime adapters.
    #[must_use]
    pub fn inner(&self) -> Arc<Mutex<BackgroundTaskSupervisor>> {
        self.inner.clone()
    }
}

impl Sealed for BackgroundSupervisorHandle {}

#[cfg(test)]
mod tests {
    use eos_tools::ToolResult;

    use super::*;

    #[test]
    fn parent_exit_then_cancel_finish_race() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let running = supervisor.register_running(
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
            Some("agent".to_owned()),
        );
        supervisor.terminate_for_parent_exit();
        let record = supervisor.get(&running).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Cancelled);
        assert!(record.outstanding());

        let racing = supervisor.register_running(
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
            Some("agent".to_owned()),
        );
        // A cancel racing a finish resolves to Completed via the precedence latch.
        supervisor.cancel_subagent(&racing, "no longer needed");
        supervisor.settle(
            &racing,
            BackgroundTaskStatus::Completed,
            ToolResult::ok("finished anyway"),
        );
        let record = supervisor.get(&racing).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Completed);
        assert_eq!(
            record.result.as_ref().map(|result| result.output.as_str()),
            Some("finished anyway")
        );

        // Both records left Running, so the agent-scoped count is zero.
        assert_eq!(supervisor.inflight_report("agent").subagent, 0);
        assert_eq!(supervisor.inflight_report("agent").total, 0);
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
            supervisor.register_running(JsonObject::new(), BackgroundTaskKind::Subagent, None),
            "subagent_1"
        );
        assert_eq!(
            supervisor.register_running(JsonObject::new(), BackgroundTaskKind::Workflow, None),
            "wf_1"
        );
    }

    #[test]
    fn inflight_report_is_subagent_and_agent_scoped() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let a = supervisor.register_running(
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
            Some("agent-a".to_owned()),
        );
        supervisor.register_running(
            JsonObject::new(),
            BackgroundTaskKind::Subagent,
            Some("agent-b".to_owned()),
        );
        assert_eq!(supervisor.inflight_report("agent-a").subagent, 1);
        assert_eq!(supervisor.inflight_report("agent-b").subagent, 1);

        // Draining agent-a settles only its subagent; agent-b is untouched.
        let report = supervisor.drain_subagents_for_agent("agent-a");
        assert_eq!(report.subagent, 0);
        assert_eq!(
            supervisor.get(&a).expect("record").status,
            BackgroundTaskStatus::Cancelled
        );
        assert_eq!(supervisor.inflight_report("agent-b").subagent, 1);
    }
}
