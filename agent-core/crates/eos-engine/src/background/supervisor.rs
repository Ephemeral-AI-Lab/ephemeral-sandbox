//! Background task supervisor — the single owner of every background kind:
//! subagents, workflows, and commands. Each background kind has a typed ledger,
//! while this module keeps one precedence latch, count surface
//! ([`BackgroundInflightReport`]), and parent-exit cleanup path.
//!
//! [`BackgroundSupervisorHandle`] wraps that state and is the real
//! [`BackgroundSupervisorPort`](eos_tools::ports::BackgroundSupervisorPort) (impl in
//! `subagent.rs`) and [`CommandSessionSupervisorPort`](eos_tools::ports::CommandSessionSupervisorPort)
//! (impl in `command_session.rs`). It also holds the [`EngineRunHandles`] +
//! [`AuditSink`] + [`Clock`] the subagent driver needs.

use std::collections::HashMap;
use std::sync::Arc;

use eos_audit::AuditSink;
use eos_sandbox_api::{
    CommandSessionCancelRequest, SandboxCaller, SandboxRequestBase, SandboxTransport,
};
use eos_tools::ports::Sealed;
use eos_tools::{BackgroundInflightReport, StartedWorkflow, ToolResult, WorkflowControlPort};
use eos_types::{
    Clock, CommandSessionId, JsonObject, SandboxId, SubagentSessionId, TaskId, WorkflowId,
    WorkflowSessionId,
};
use serde_json::{json, Value};
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

/// One tracked subagent run. `Debug, Clone, PartialEq` are preserved for tests;
/// the non-cloneable [`AbortHandle`] rides in a side map on the supervisor, not
/// on the record.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentRecord {
    /// Agent-facing supervisor id.
    pub subagent_session_id: SubagentSessionId,
    /// Original tool input.
    pub tool_input: JsonObject,
    /// Current status.
    pub status: BackgroundTaskStatus,
    /// Owning agent id (the launching agent), for the agent-scoped count.
    pub agent_id: Option<String>,
    /// Final result.
    pub result: Option<ToolResult>,
}

impl SubagentRecord {
    /// Whether the task still needs delivery.
    #[must_use]
    pub const fn outstanding(&self) -> bool {
        matches!(self.status, BackgroundTaskStatus::Running)
            || self.status.is_terminal_undelivered()
    }

    /// Cancel this record in-place. External side effects (aborting the subagent
    /// driver) are handled by the supervisor handle; this method owns the shared
    /// status/result transition.
    pub fn cancel(&mut self, reason: &str) -> bool {
        if !matches!(self.status, BackgroundTaskStatus::Running) {
            return false;
        }
        self.status = BackgroundTaskStatus::Cancelled;
        self.result = Some(
            ToolResult::error(format!("Background subagent cancelled: {reason}"))
                .meta("subagent_cancelled", json!(true)),
        );
        true
    }
}

/// One delegated workflow handle tracked as background work. Persisted workflow
/// lifecycle remains owned by [`WorkflowControlPort`]; this record owns
/// parent-exit cleanup and in-flight counts.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowBackgroundRecord {
    /// Agent-facing workflow handle.
    pub workflow_task_id: WorkflowSessionId,
    /// Persisted workflow id.
    pub workflow_id: WorkflowId,
    /// Parent task that launched this workflow.
    pub parent_task_id: TaskId,
    /// Goal text submitted to `delegate_workflow`.
    pub workflow_goal: String,
    /// Current supervisor status.
    pub status: BackgroundTaskStatus,
    /// Owning agent id (the launching agent), for the agent-scoped count.
    pub agent_id: String,
    /// Final supervisor-side result.
    pub result: Option<ToolResult>,
}

impl WorkflowBackgroundRecord {
    /// Cancel this workflow record in-place. Persisted state cancellation is
    /// handled by [`WorkflowControlPort`].
    pub fn cancel(&mut self, reason: &str) -> bool {
        if !matches!(self.status, BackgroundTaskStatus::Running) {
            return false;
        }
        self.status = BackgroundTaskStatus::Cancelled;
        self.result = Some(
            ToolResult::error(format!("Delegated workflow cancelled: {reason}"))
                .meta("workflow_cancelled", json!(true)),
        );
        true
    }
}

#[derive(Debug, Clone)]
pub(super) struct CommandSessionCancelTarget {
    pub command_session_id: CommandSessionId,
    pub sandbox_id: SandboxId,
    pub agent_id: String,
}

/// Single-owner background supervisor state.
#[derive(Debug, Default)]
pub struct BackgroundTaskSupervisor {
    subagent_counter: u64,
    subagents: HashMap<SubagentSessionId, SubagentRecord>,
    workflows: HashMap<WorkflowSessionId, WorkflowBackgroundRecord>,
    /// Abort handles for running subagent task drivers, keyed by
    /// `subagent_session_id`.
    /// Resource hygiene only — what unwedges the terminal is the *settle* (the
    /// record leaves `Running`); `abort()` merely stops a runaway child.
    handles: HashMap<SubagentSessionId, AbortHandle>,
    /// Tracked background PTY command sessions, keyed by daemon-minted
    /// `command_session_id` (anchor §5). Visible to the sibling
    /// `command_session` module that owns their lifecycle methods.
    pub(super) commands: HashMap<CommandSessionId, super::command_session::CommandSessionRecord>,
}

impl BackgroundTaskSupervisor {
    /// Create an empty supervisor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a running subagent, stamping the owning `agent_id`.
    pub fn register_subagent(
        &mut self,
        tool_input: JsonObject,
        agent_id: Option<String>,
    ) -> SubagentSessionId {
        self.subagent_counter = self.subagent_counter.saturating_add(1);
        let subagent_session_id: SubagentSessionId =
            match format!("subagent_{}", self.subagent_counter).parse() {
                Ok(id) => id,
                Err(_) => unreachable!("generated subagent ids are non-empty"),
            };
        self.subagents.insert(
            subagent_session_id.clone(),
            SubagentRecord {
                subagent_session_id: subagent_session_id.clone(),
                tool_input,
                status: BackgroundTaskStatus::Running,
                agent_id,
                result: None,
            },
        );
        subagent_session_id
    }

    /// Register a delegated workflow as background work. The workflow-control
    /// adapter owns the persisted state; this supervisor owns the handle bookkeeping
    /// used by background counts and parent-exit cleanup.
    pub fn register_workflow(
        &mut self,
        parent_task_id: &TaskId,
        agent_id: &str,
        workflow_goal: &str,
        workflow: &StartedWorkflow,
    ) {
        self.workflows.insert(
            workflow.workflow_task_id.clone(),
            WorkflowBackgroundRecord {
                workflow_task_id: workflow.workflow_task_id.clone(),
                workflow_id: workflow.workflow_id.clone(),
                parent_task_id: parent_task_id.clone(),
                workflow_goal: workflow_goal.to_owned(),
                status: BackgroundTaskStatus::Running,
                agent_id: agent_id.to_owned(),
                result: None,
            },
        );
    }

    /// Borrow a subagent record.
    #[must_use]
    pub fn get_subagent(&self, subagent_session_id: &SubagentSessionId) -> Option<&SubagentRecord> {
        self.subagents.get(subagent_session_id)
    }

    /// Settle a subagent record to a terminal status with its result, gated by the
    /// precedence latch (Python `_done_callback` / `_apply_terminal_status_transition`):
    /// a higher-precedence outcome wins, so a finish racing a cancel resolves to
    /// `Completed`. This is the single on-completion routine for the subagent
    /// driver — the status is classified by terminal *presence* (Completed when a
    /// terminal was called, even if its `is_error` is true), not by `is_error`.
    pub fn settle_subagent(
        &mut self,
        subagent_session_id: &SubagentSessionId,
        status: BackgroundTaskStatus,
        result: ToolResult,
    ) {
        if let Some(record) = self.subagents.get_mut(subagent_session_id) {
            if status.precedence() > record.status.precedence() {
                record.status = status;
                record.result = Some(result);
            }
        }
    }

    /// Cancel one tracked subagent, settling it `Cancelled`. Returns `false` for
    /// an unknown or already-settled session (Python `cancel_subagent_session`).
    pub fn cancel_subagent(
        &mut self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> bool {
        self.subagents
            .get_mut(subagent_session_id)
            .is_some_and(|record| record.cancel(reason))
    }

    /// Mark one tracked workflow as cancelled in the supervisor ledger.
    pub fn cancel_workflow_record(
        &mut self,
        workflow_task_id: &WorkflowSessionId,
        reason: &str,
    ) -> bool {
        self.workflows
            .get_mut(workflow_task_id)
            .is_some_and(|record| record.cancel(reason))
    }

    /// Cancel this agent's in-flight subagent runs (settle `Cancelled` + abort
    /// the drivers), then return the post-cancel report. The terminal/exit
    /// prehook runs this so a live or phantom subagent never wedges the agent's
    /// terminal (D9). Workflows and commands have separate cleanup paths.
    pub fn cancel_subagents_for_agent(&mut self, agent_id: &str) -> BackgroundInflightReport {
        let ids: Vec<SubagentSessionId> = self
            .subagents
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id.as_deref() == Some(agent_id))
            })
            .map(|record| record.subagent_session_id.clone())
            .collect();
        for id in ids {
            if let Some(record) = self.subagents.get_mut(&id) {
                record.cancel("parent submitted its terminal");
            }
            self.take_and_abort_handle(&id);
        }
        self.inflight_report(agent_id)
    }

    /// Running workflow handles for this agent, used by parent-exit cleanup.
    #[must_use]
    pub fn running_workflows_for_agent(&self, agent_id: &str) -> Vec<WorkflowSessionId> {
        self.workflows
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id == agent_id)
            })
            .map(|record| record.workflow_task_id.clone())
            .collect()
    }

    /// Running commands for this agent, used by parent-exit cleanup.
    #[must_use]
    pub(super) fn running_commands_for_agent(
        &self,
        agent_id: &str,
    ) -> Vec<CommandSessionCancelTarget> {
        self.commands
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id == agent_id)
            })
            .map(|record| CommandSessionCancelTarget {
                command_session_id: record.command_session_id.clone(),
                sandbox_id: record.sandbox_id.clone(),
                agent_id: record.agent_id.clone(),
            })
            .collect()
    }

    /// Mark one command record cancelled after parent-exit cleanup asks the daemon
    /// to stop it.
    pub(super) fn cancel_command_record(&mut self, command_session_id: &CommandSessionId) -> bool {
        let Some(record) = self.commands.get_mut(command_session_id) else {
            return false;
        };
        if !matches!(record.status, BackgroundTaskStatus::Running) {
            return false;
        }
        record.status = BackgroundTaskStatus::Cancelled;
        record.result = Some(json!({
            "status": "cancelled",
            "exit_code": Value::Null,
            "output": {"stdout": "", "stderr": ""},
        }));
        true
    }

    /// This agent's in-flight background report (Running-only): subagents,
    /// workflows, and commands. An empty `agent_id` counts all.
    #[must_use]
    pub fn inflight_report(&self, agent_id: &str) -> BackgroundInflightReport {
        let subagent = self
            .subagents
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id.as_deref() == Some(agent_id))
            })
            .count();
        let workflow = self
            .workflows
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && (agent_id.is_empty() || record.agent_id == agent_id)
            })
            .count();
        let command_session = self.count_commands_by_agent(agent_id);
        BackgroundInflightReport {
            total: subagent + workflow + command_session,
            subagent,
            workflow,
            command_session,
        }
    }

    /// Store a running subagent driver's abort handle.
    pub fn store_handle(&mut self, subagent_session_id: SubagentSessionId, handle: AbortHandle) {
        self.handles.insert(subagent_session_id, handle);
    }

    /// Drop a finished driver's handle without aborting (the run already ended).
    pub fn forget_handle(&mut self, subagent_session_id: &SubagentSessionId) {
        self.handles.remove(subagent_session_id);
    }

    /// Abort and drop a running driver's handle.
    pub fn take_and_abort_handle(&mut self, subagent_session_id: &SubagentSessionId) {
        if let Some(handle) = self.handles.remove(subagent_session_id) {
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
    transport: Arc<dyn SandboxTransport>,
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
        transport: Arc<dyn SandboxTransport>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackgroundTaskSupervisor::new())),
            handles,
            audit,
            clock,
            transport,
        }
    }

    /// Access the shared supervisor state for the heartbeat and runtime adapters.
    #[must_use]
    pub fn inner(&self) -> Arc<Mutex<BackgroundTaskSupervisor>> {
        self.inner.clone()
    }

    /// Cancel all background work tracked for one parent agent. This is the common
    /// parent-exit finalizer for `ToolStop`, terminal exhaustion, and engine faults.
    pub async fn cancel_for_parent_exit(
        &self,
        agent_id: &str,
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        reason: &str,
    ) -> BackgroundInflightReport {
        let (workflows, commands) = {
            let mut guard = self.inner.lock().await;
            guard.cancel_subagents_for_agent(agent_id);
            (
                guard.running_workflows_for_agent(agent_id),
                guard.running_commands_for_agent(agent_id),
            )
        };

        for workflow_task_id in workflows {
            if let Some(control) = &workflow_control {
                if let Err(err) = control.cancel(&workflow_task_id, reason).await {
                    tracing::warn!(
                        error = %err,
                        workflow_task_id = workflow_task_id.as_str(),
                        "background workflow parent-exit cancellation failed"
                    );
                }
            }
            self.inner
                .lock()
                .await
                .cancel_workflow_record(&workflow_task_id, reason);
        }

        for command in commands {
            self.cancel_command_session_for_parent_exit(&command, reason)
                .await;
            self.inner
                .lock()
                .await
                .cancel_command_record(&command.command_session_id);
        }

        self.inner.lock().await.inflight_report(agent_id)
    }

    async fn cancel_command_session_for_parent_exit(
        &self,
        command: &CommandSessionCancelTarget,
        reason: &str,
    ) {
        let request = CommandSessionCancelRequest {
            base: SandboxRequestBase {
                caller: SandboxCaller {
                    agent_id: command.agent_id.clone(),
                    run_id: command.agent_id.clone(),
                    agent_run_id: command.agent_id.clone(),
                    task_id: String::new(),
                    request_id: String::new(),
                    attempt_id: String::new(),
                    workflow_id: String::new(),
                    tool_id: None,
                },
                description: format!("parent-exit cleanup: {reason}"),
                invocation_id: None,
            },
            command_session_id: command.command_session_id.as_str().to_owned(),
        };
        if let Err(err) =
            eos_sandbox_api::cancel_command_session(&*self.transport, &command.sandbox_id, &request)
                .await
        {
            tracing::warn!(
                error = %err,
                command_session_id = command.command_session_id.as_str(),
                "background command-session parent-exit cancellation failed"
            );
        }
    }
}

impl Sealed for BackgroundSupervisorHandle {}

#[cfg(test)]
mod tests {
    use eos_tools::{StartedWorkflow, ToolResult};

    use super::*;

    #[test]
    fn parent_exit_then_cancel_finish_race() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let running = supervisor.register_subagent(JsonObject::new(), Some("agent".to_owned()));
        supervisor.cancel_subagents_for_agent("agent");
        let record = supervisor.get_subagent(&running).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Cancelled);
        assert!(record.outstanding());

        let racing = supervisor.register_subagent(JsonObject::new(), Some("agent".to_owned()));
        // A cancel racing a finish resolves to Completed via the precedence latch.
        supervisor.cancel_subagent(&racing, "no longer needed");
        supervisor.settle_subagent(
            &racing,
            BackgroundTaskStatus::Completed,
            ToolResult::ok("finished anyway"),
        );
        let record = supervisor.get_subagent(&racing).expect("record exists");
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
            supervisor
                .register_subagent(JsonObject::new(), None)
                .as_str(),
            "subagent_1"
        );
    }

    #[test]
    fn workflow_registration_is_counted_and_cancellable() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let workflow = StartedWorkflow {
            workflow_id: eos_types::WorkflowId::new_v4(),
            workflow_task_id: "wf_1".parse().expect("workflow handle"),
        };
        supervisor.register_workflow(
            &"parent".parse().expect("parent task id"),
            "agent-a",
            "delegate this",
            &workflow,
        );
        assert_eq!(
            supervisor.inflight_report("agent-a").workflow,
            1,
            "workflow handles are background-supervisor-aware"
        );
        assert!(
            supervisor.cancel_workflow_record(&workflow.workflow_task_id, "parent exited"),
            "workflow record should share the generalized cancel transition"
        );
        assert_eq!(
            supervisor.inflight_report("agent-a").workflow,
            0,
            "cancelled workflow leaves the running count"
        );
    }

    #[test]
    fn inflight_report_is_subagent_and_agent_scoped() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let a = supervisor.register_subagent(JsonObject::new(), Some("agent-a".to_owned()));
        supervisor.register_subagent(JsonObject::new(), Some("agent-b".to_owned()));
        assert_eq!(supervisor.inflight_report("agent-a").subagent, 1);
        assert_eq!(supervisor.inflight_report("agent-b").subagent, 1);

        // Cancelling agent-a settles only its subagent; agent-b is untouched.
        let report = supervisor.cancel_subagents_for_agent("agent-a");
        assert_eq!(report.subagent, 0);
        assert_eq!(
            supervisor.get_subagent(&a).expect("record").status,
            BackgroundTaskStatus::Cancelled
        );
        assert_eq!(supervisor.inflight_report("agent-b").subagent, 1);
    }
}
