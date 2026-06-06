//! Background task supervisor state — [`BackgroundTaskSupervisor`], the single
//! synchronous ledger owning every background kind (subagents, workflows, and
//! commands), the precedence latch, and the count surface
//! ([`BackgroundInflightReport`]). The async wrapper that drives the supervisor
//! ports and parent-exit cleanup lives in
//! [`BackgroundSupervisorHandle`](super::BackgroundSupervisorHandle).

use std::collections::HashMap;

use eos_tools::{BackgroundInflightReport, StartedWorkflowHandle, ToolResult};
use eos_types::{
    AgentRunId, CommandSessionId, JsonObject, SandboxId, SubagentSessionId, WorkflowSessionId,
};
use serde_json::{json, Value};
use tokio::task::AbortHandle;

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
    /// Owning agent run (the launching agent execution), for run-scoped counts.
    pub agent_run_id: AgentRunId,
    /// Final result.
    pub result: Option<ToolResult>,
}

impl SubagentRecord {
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
    /// Current supervisor status.
    pub status: BackgroundTaskStatus,
    /// Owning agent run (the launching agent execution), for run-scoped counts.
    pub agent_run_id: AgentRunId,
}

impl WorkflowBackgroundRecord {
    /// Cancel this workflow record in-place. Persisted state cancellation is
    /// handled by [`WorkflowControlPort`].
    pub fn cancel(&mut self, _reason: &str) -> bool {
        if !matches!(self.status, BackgroundTaskStatus::Running) {
            return false;
        }
        self.status = BackgroundTaskStatus::Cancelled;
        true
    }
}

#[derive(Debug, Clone)]
pub(super) struct CommandSessionCancelTarget {
    pub command_session_id: CommandSessionId,
    pub sandbox_id: SandboxId,
    pub agent_run_id: AgentRunId,
}

pub(super) fn matches_agent_run(recorded: &AgentRunId, scope: Option<&AgentRunId>) -> bool {
    match scope {
        Some(agent_run_id) => recorded == agent_run_id,
        None => true,
    }
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

    /// Register a running subagent, stamping the owning `agent_run_id`.
    pub fn register_subagent(
        &mut self,
        tool_input: JsonObject,
        agent_run_id: AgentRunId,
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
                agent_run_id,
                result: None,
            },
        );
        subagent_session_id
    }

    /// Register a delegated workflow as background work. The workflow-control
    /// adapter owns persisted state; this supervisor only tracks the handle for
    /// counts and parent-exit cleanup.
    pub fn register_workflow(
        &mut self,
        agent_run_id: &AgentRunId,
        workflow: &StartedWorkflowHandle,
    ) {
        self.workflows.insert(
            workflow.workflow_task_id.clone(),
            WorkflowBackgroundRecord {
                workflow_task_id: workflow.workflow_task_id.clone(),
                status: BackgroundTaskStatus::Running,
                agent_run_id: agent_run_id.clone(),
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

    /// Cancel this agent run's in-flight subagent runs (settle `Cancelled` + abort
    /// the drivers), then return the post-cancel report. The terminal/exit
    /// prehook runs this so a live or phantom subagent never wedges the agent's
    /// terminal (D9). Workflows and commands have separate cleanup paths.
    pub fn cancel_subagents_for_agent_run(
        &mut self,
        agent_run_id: &AgentRunId,
    ) -> BackgroundInflightReport {
        let ids: Vec<SubagentSessionId> = self
            .subagents
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && record.agent_run_id == *agent_run_id
            })
            .map(|record| record.subagent_session_id.clone())
            .collect();
        for id in ids {
            if let Some(record) = self.subagents.get_mut(&id) {
                record.cancel("parent submitted its terminal");
            }
            self.take_and_abort_handle(&id);
        }
        self.inflight_report(Some(agent_run_id))
    }

    /// Running workflow handles for this agent run, used by parent-exit cleanup.
    #[must_use]
    pub fn running_workflows_for_agent_run(
        &self,
        agent_run_id: Option<&AgentRunId>,
    ) -> Vec<WorkflowSessionId> {
        self.workflows
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches_agent_run(&record.agent_run_id, agent_run_id)
            })
            .map(|record| record.workflow_task_id.clone())
            .collect()
    }

    /// Running commands for this agent run, used by parent-exit cleanup.
    #[must_use]
    pub(super) fn running_commands_for_agent_run(
        &self,
        agent_run_id: Option<&AgentRunId>,
    ) -> Vec<CommandSessionCancelTarget> {
        self.commands
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches_agent_run(&record.agent_run_id, agent_run_id)
            })
            .map(|record| CommandSessionCancelTarget {
                command_session_id: record.command_session_id.clone(),
                sandbox_id: record.sandbox_id.clone(),
                agent_run_id: record.agent_run_id.clone(),
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

    /// This agent run's in-flight background report (Running-only): subagents,
    /// workflows, and commands. `None` counts all.
    #[must_use]
    pub fn inflight_report(&self, agent_run_id: Option<&AgentRunId>) -> BackgroundInflightReport {
        let subagent = self
            .subagents
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches_agent_run(&record.agent_run_id, agent_run_id)
            })
            .count();
        let workflow = self
            .workflows
            .values()
            .filter(|record| {
                matches!(record.status, BackgroundTaskStatus::Running)
                    && matches_agent_run(&record.agent_run_id, agent_run_id)
            })
            .count();
        let command_session = self.count_commands_by_run(agent_run_id);
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

#[cfg(test)]
mod tests {
    use eos_tools::{StartedWorkflowHandle, ToolResult};

    use super::*;

    fn agent_run_id(value: &str) -> AgentRunId {
        value.parse().expect("agent run id")
    }

    #[test]
    fn parent_exit_then_cancel_finish_race() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let agent = agent_run_id("agent");
        let running = supervisor.register_subagent(JsonObject::new(), agent.clone());
        supervisor.cancel_subagents_for_agent_run(&agent);
        let record = supervisor.get_subagent(&running).expect("record exists");
        assert_eq!(record.status, BackgroundTaskStatus::Cancelled);
        assert!(record.result.is_some());

        let racing = supervisor.register_subagent(JsonObject::new(), agent.clone());
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

        // Both records left Running, so the agent-run-scoped count is zero.
        assert_eq!(supervisor.inflight_report(Some(&agent)).subagent, 0);
        assert_eq!(supervisor.inflight_report(Some(&agent)).total, 0);
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
                .register_subagent(JsonObject::new(), agent_run_id("agent"))
                .as_str(),
            "subagent_1"
        );
    }

    #[test]
    fn workflow_registration_is_counted_and_cancellable() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let workflow = StartedWorkflowHandle {
            workflow_id: eos_types::WorkflowId::new_v4(),
            workflow_task_id: "wf_1".parse().expect("workflow handle"),
        };
        let agent = agent_run_id("agent-a");
        supervisor.register_workflow(&agent, &workflow);
        assert_eq!(
            supervisor.inflight_report(Some(&agent)).workflow,
            1,
            "workflow handles are background-supervisor-aware"
        );
        assert!(
            supervisor.cancel_workflow_record(&workflow.workflow_task_id, "parent exited"),
            "workflow record should share the generalized cancel transition"
        );
        assert_eq!(
            supervisor.inflight_report(Some(&agent)).workflow,
            0,
            "cancelled workflow leaves the running count"
        );
    }

    #[test]
    fn inflight_report_is_subagent_and_agent_run_scoped() {
        let mut supervisor = BackgroundTaskSupervisor::new();
        let agent_a = agent_run_id("agent-a");
        let agent_b = agent_run_id("agent-b");
        let a = supervisor.register_subagent(JsonObject::new(), agent_a.clone());
        supervisor.register_subagent(JsonObject::new(), agent_b.clone());
        assert_eq!(supervisor.inflight_report(Some(&agent_a)).subagent, 1);
        assert_eq!(supervisor.inflight_report(Some(&agent_b)).subagent, 1);

        // Cancelling agent-a's run settles only its subagent; agent-b is untouched.
        let report = supervisor.cancel_subagents_for_agent_run(&agent_a);
        assert_eq!(report.subagent, 0);
        assert_eq!(
            supervisor.get_subagent(&a).expect("record").status,
            BackgroundTaskStatus::Cancelled
        );
        assert_eq!(supervisor.inflight_report(Some(&agent_b)).subagent, 1);
    }
}
