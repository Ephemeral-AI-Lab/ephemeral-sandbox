//! [`ExecutionMetadata`] — the typed runtime context threaded through every tool
//! execution.
//!
//! Ports `_framework/core/runtime.py::ExecutionMetadata`, **dropping** the
//! `Mapping`-emulation shim (`get`/`__getitem__`/`__iter__`/`extras`/
//! `_TYPED_FIELDS`) — that was migration scaffolding. IDs become newtypes;
//! downstream services become injected port-trait objects (§5.6).
//!
//! **Deliberate deviation from spec §6.4 (documented):** the table lists only
//! `task_store`, but `submit_root_outcome` also *finishes the request*, and in
//! `eos-state` `finish_request` lives on `RequestStore` (the ISP split of the
//! Python `TaskStore`). So this struct carries **both** `task_store` and
//! `request_store`. The Python `tool_registry` field is intentionally dropped
//! (it existed only for skills introspecting sibling tools — out of scope).
//! `runtime_config`/`composer`/`attempt_runtime`/`conversation_messages`/
//! `context_preparers`/`on_progress_line`/`background_task_id` are engine
//! plumbing (moved to `eos-engine` dispatch context), not tool-facing.

use std::sync::Arc;

use eos_sandbox_api::{SandboxCaller, SandboxTransport};
use eos_skills::SkillRegistry;
use eos_state::{RequestStore, TaskStore};
use eos_types::{
    AgentRunId, AttemptId, InvocationId, RequestId, SandboxId, TaskId, ToolUseId, WorkflowId,
};

use crate::error::ToolError;
use crate::ports::{
    AdvisorPort, IsolatedWorkspacePort, NotificationSink, PlanSubmissionPort,
    SubagentSupervisorPort, WorkflowControlPort,
};

/// The typed bag of runtime context a tool executor reads. Built per tool call
/// and owned by the call (no shared mutation); ports are `Arc<dyn _>` cloned
/// cheaply.
#[derive(Clone)]
pub struct ExecutionMetadata {
    /// Provisioned sandbox, when the agent is sandbox-bound.
    pub sandbox_id: Option<SandboxId>,
    /// Agent-run id (engine agent factory).
    pub agent_run_id: Option<AgentRunId>,
    /// Bound agent profile name.
    pub agent_name: String,
    /// Working directory.
    pub cwd: String,
    /// Repository root.
    pub repo_root: String,
    /// Effective exec cwd.
    pub exec_cwd: String,
    /// Owning request, when set.
    pub request_id: Option<RequestId>,
    /// Owning task, when set.
    pub task_id: Option<TaskId>,
    /// Owning attempt, when set.
    pub attempt_id: Option<AttemptId>,
    /// Owning workflow, when set.
    pub workflow_id: Option<WorkflowId>,
    /// Per-call tool-use id (set by the streaming executor upstream).
    pub tool_use_id: Option<ToolUseId>,
    /// In-flight sandbox correlation id, when set.
    pub sandbox_invocation_id: Option<InvocationId>,
    /// Caller identity for sandbox calls (`sandbox_caller_from_tool_context`).
    pub caller: SandboxCaller,
    /// The sandbox RPC surface (`sandbox_api.*`).
    pub transport: Arc<dyn SandboxTransport>,
    /// Task persistence (`submit_root_outcome` and task lookups).
    pub task_store: Arc<dyn TaskStore>,
    /// Request persistence (`submit_root_outcome` finishes the request).
    pub request_store: Arc<dyn RequestStore>,
    /// Per-agent skill content for `load_skill_reference`.
    pub skill_registry: Arc<SkillRegistry>,
    /// Workflow control port (delegate/check/cancel workflow).
    pub workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    /// Plan-submission port (planner/generator/reducer terminals).
    pub plan_submission: Option<Arc<dyn PlanSubmissionPort>>,
    /// Subagent supervisor port (run/check/cancel subagent + bg count).
    pub subagent_supervisor: Option<Arc<dyn SubagentSupervisorPort>>,
    /// Advisor port (`ask_advisor` + `AdvisorApproval` hook).
    pub advisor: Option<Arc<dyn AdvisorPort>>,
    /// Isolated-workspace lifecycle port (enter/exit).
    pub isolated_workspace: Option<Arc<dyn IsolatedWorkspacePort>>,
    /// System-notification sink.
    pub notifications: Option<Arc<dyn NotificationSink>>,
}

impl std::fmt::Debug for ExecutionMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ports/stores are trait objects (no Debug); show the identifiers only.
        f.debug_struct("ExecutionMetadata")
            .field("sandbox_id", &self.sandbox_id)
            .field("agent_run_id", &self.agent_run_id)
            .field("agent_name", &self.agent_name)
            .field("request_id", &self.request_id)
            .field("task_id", &self.task_id)
            .field("attempt_id", &self.attempt_id)
            .field("workflow_id", &self.workflow_id)
            .field("tool_use_id", &self.tool_use_id)
            .finish_non_exhaustive()
    }
}

impl ExecutionMetadata {
    /// The calling agent's id: `agent_run_id` then `agent_name`, stripped
    /// (Python `resolve_agent_id`).
    #[must_use]
    pub fn agent_id(&self) -> String {
        let from_run = self
            .agent_run_id
            .as_ref()
            .map(|id| id.as_str().trim())
            .filter(|s| !s.is_empty());
        match from_run {
            Some(s) => s.to_owned(),
            None => self.agent_name.trim().to_owned(),
        }
    }

    /// The calling agent's sandbox id as a string, or `""` when unbound (Python
    /// `resolve_sandbox_id`).
    #[must_use]
    pub fn sandbox_id_str(&self) -> &str {
        self.sandbox_id.as_ref().map_or("", SandboxId::as_str)
    }

    /// Require the bound sandbox id, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingContext`] when no sandbox is bound.
    pub fn require_sandbox_id(&self) -> Result<&SandboxId, ToolError> {
        self.sandbox_id
            .as_ref()
            .ok_or(ToolError::MissingContext("sandbox_id"))
    }

    /// Require the owning task id, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingContext`] when no task id is set.
    pub fn require_task_id(&self) -> Result<&TaskId, ToolError> {
        self.task_id
            .as_ref()
            .ok_or(ToolError::MissingContext("task_id"))
    }

    /// Require the owning request id, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingContext`] when no request id is set.
    pub fn require_request_id(&self) -> Result<&RequestId, ToolError> {
        self.request_id
            .as_ref()
            .ok_or(ToolError::MissingContext("request_id"))
    }

    /// Require the owning attempt id, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingContext`] when no attempt id is set.
    pub fn require_attempt_id(&self) -> Result<&AttemptId, ToolError> {
        self.attempt_id
            .as_ref()
            .ok_or(ToolError::MissingContext("attempt_id"))
    }

    /// Require the workflow-control port, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingPort`] when the port is not wired.
    pub fn require_workflow_control(&self) -> Result<&dyn WorkflowControlPort, ToolError> {
        self.workflow_control
            .as_deref()
            .ok_or(ToolError::MissingPort("workflow_control"))
    }

    /// Require the plan-submission port, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingPort`] when the port is not wired.
    pub fn require_plan_submission(&self) -> Result<&dyn PlanSubmissionPort, ToolError> {
        self.plan_submission
            .as_deref()
            .ok_or(ToolError::MissingPort("plan_submission"))
    }

    /// Require the subagent supervisor port, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingPort`] when the port is not wired.
    pub fn require_subagent_supervisor(&self) -> Result<&dyn SubagentSupervisorPort, ToolError> {
        self.subagent_supervisor
            .as_deref()
            .ok_or(ToolError::MissingPort("subagent_supervisor"))
    }

    /// Require the advisor port, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingPort`] when the port is not wired.
    pub fn require_advisor(&self) -> Result<&dyn AdvisorPort, ToolError> {
        self.advisor
            .as_deref()
            .ok_or(ToolError::MissingPort("advisor"))
    }

    /// Require the isolated-workspace port, else a framework fault.
    ///
    /// # Errors
    /// [`ToolError::MissingPort`] when the port is not wired.
    pub fn require_isolated_workspace(&self) -> Result<&dyn IsolatedWorkspacePort, ToolError> {
        self.isolated_workspace
            .as_deref()
            .ok_or(ToolError::MissingPort("isolated_workspace"))
    }
}
