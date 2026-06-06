//! Build the typed [`ExecutionMetadata`] threaded into every tool call, shared
//! by the root-agent and delegated-workflow paths.

use std::sync::Arc;

use eos_tools::ExecutionMetadata;
use eos_types::{AgentRunId, AttemptId, RequestId, SandboxId, TaskId, WorkflowId};

/// The per-run identifiers and ports that distinguish one agent's tool context.
pub(crate) struct MetadataParams {
    pub agent_name: String,
    pub sandbox_id: Option<SandboxId>,
    pub agent_run_id: AgentRunId,
    pub request_id: Option<RequestId>,
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<AttemptId>,
    pub workflow_id: Option<WorkflowId>,
    pub is_isolated_workspace_mode: bool,
}

/// Assemble the tool execution context from request-scoped workspace facts plus
/// per-run ids. The `conversation` transcript starts empty here and is stamped
/// per-call by the engine dispatch before any hook reads it.
pub(crate) fn build_metadata(workspace_root: &str, params: MetadataParams) -> ExecutionMetadata {
    ExecutionMetadata {
        sandbox_id: params.sandbox_id,
        agent_run_id: Some(params.agent_run_id),
        agent_name: params.agent_name,
        request_id: params.request_id,
        task_id: params.task_id,
        attempt_id: params.attempt_id,
        workflow_id: params.workflow_id,
        tool_use_id: None,
        sandbox_invocation_id: None,
        is_isolated_workspace_mode: params.is_isolated_workspace_mode,
        workspace_root: workspace_root.to_owned(),
        conversation: Arc::from(Vec::new()),
    }
}
