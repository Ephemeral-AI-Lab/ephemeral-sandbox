//! Build the typed [`ExecutionMetadata`] threaded into every tool call, shared
//! by the root-agent and delegated-workflow paths.

use std::sync::Arc;

use eos_sandbox_api::SandboxCaller;
use eos_tools::{
    CommandSessionSupervisorPort, ExecutionMetadata, NotificationSink, PlanSubmissionPort,
    SubagentSupervisorPort, WorkflowControlPort,
};
use eos_types::{AgentRunId, AttemptId, RequestId, SandboxId, TaskId, WorkflowId};

use crate::app_state::AppState;

/// The per-run identifiers and ports that distinguish one agent's tool context.
pub(crate) struct MetadataParams {
    pub agent_name: String,
    pub sandbox_id: Option<SandboxId>,
    pub agent_run_id: AgentRunId,
    pub request_id: Option<RequestId>,
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<AttemptId>,
    pub workflow_id: Option<WorkflowId>,
    /// Wired for the root agent (delegate/check/cancel workflow); `None` for
    /// workflow agents in Phase 6 (nested delegation is deferred).
    pub workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    /// The recording plan-submission port (planner/generator/reducer terminals).
    /// Wired for delegated-workflow agents so their submit tools record straight
    /// to the orchestrator (Path A-recording); `None` for the root agent.
    pub plan_submission: Option<Arc<dyn PlanSubmissionPort>>,
    pub subagent_supervisor: Option<Arc<dyn SubagentSupervisorPort>>,
    /// The per-request command-session supervisor port (anchor §5), shared with
    /// the heartbeat and loop notifier.
    pub command_session_supervisor: Option<Arc<dyn CommandSessionSupervisorPort>>,
    /// The per-request notification sink the `exec_command`/`write_stdin` tools
    /// push to and the loop drains (anchor §7 instance identity).
    pub notifications: Arc<dyn NotificationSink>,
}

/// Assemble the tool execution context from the shared app state plus per-run
/// params. `plan_submission` and `isolated_workspace` are intentionally `None`
/// (Phase-6 scope); advisor/notifications come from the shared engine services.
pub(crate) fn build_metadata(state: &AppState, params: MetadataParams) -> ExecutionMetadata {
    let caller = SandboxCaller {
        agent_id: params.agent_name.clone(),
        run_id: params.agent_run_id.as_str().to_owned(),
        agent_run_id: params.agent_run_id.as_str().to_owned(),
        task_id: params
            .task_id
            .as_ref()
            .map(TaskId::as_str)
            .unwrap_or_default()
            .to_owned(),
        request_id: params
            .request_id
            .as_ref()
            .map(RequestId::as_str)
            .unwrap_or_default()
            .to_owned(),
        attempt_id: params
            .attempt_id
            .as_ref()
            .map(AttemptId::as_str)
            .unwrap_or_default()
            .to_owned(),
        workflow_id: params
            .workflow_id
            .as_ref()
            .map(WorkflowId::as_str)
            .unwrap_or_default()
            .to_owned(),
        tool_id: None,
    };

    ExecutionMetadata {
        sandbox_id: params.sandbox_id,
        agent_run_id: Some(params.agent_run_id),
        agent_name: params.agent_name,
        cwd: state.cwd.clone(),
        repo_root: state.repo_root.clone(),
        exec_cwd: state.cwd.clone(),
        request_id: params.request_id,
        task_id: params.task_id,
        attempt_id: params.attempt_id,
        workflow_id: params.workflow_id,
        tool_use_id: None,
        sandbox_invocation_id: None,
        caller,
        transport: state.transport.clone(),
        task_store: state.task_store.clone(),
        request_store: state.request_store.clone(),
        skill_registry: state.skill_registry.clone(),
        workflow_control: params.workflow_control,
        plan_submission: None,
        subagent_supervisor: params.subagent_supervisor,
        command_session_supervisor: params.command_session_supervisor,
        advisor: Some(state.advisor.clone()),
        isolated_workspace: None,
        notifications: Some(params.notifications),
    }
}
