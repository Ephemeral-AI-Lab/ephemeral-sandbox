use eos_types::{
    AgentRunId, AttemptId, IterationId, JsonObject, Message, RequestId, TaskId, WorkflowId,
    WorkflowTaskRole,
};
use serde_json::json;

/// Input for starting an agent-run message-record node.
#[derive(Debug, Clone, Copy)]
pub struct AgentRunRecordStart<'a> {
    /// Owning request id.
    pub request_id: &'a RequestId,
    /// Owning task id, when this run is task-backed.
    pub task_id: Option<&'a TaskId>,
    /// Agent-run id.
    pub agent_run_id: &'a AgentRunId,
    /// Bound agent profile name.
    pub agent_name: &'a str,
    /// Node type and parent/location facts.
    pub kind: &'a AgentRunRecordKind,
    /// Fully assembled system prompt.
    pub system_prompt: &'a str,
    /// Seed transcript rows supplied to the agent.
    pub initial_messages: &'a [Message],
}

/// Agent-run message-record node type and location facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunRecordKind {
    /// Root request agent.
    Root,
    /// Delegated workflow planner/generator/reducer task agent.
    WorkflowTask {
        /// Owning workflow id.
        workflow_id: WorkflowId,
        /// Owning iteration id.
        iteration_id: IterationId,
        /// Owning attempt id.
        attempt_id: AttemptId,
        /// Workflow task role.
        role: WorkflowTaskRole,
    },
    /// Background subagent run under a parent agent.
    Subagent {
        /// Parent agent-run id.
        parent_agent_run_id: AgentRunId,
    },
    /// Advisor run under a parent agent.
    Advisor {
        /// Parent agent-run id.
        parent_agent_run_id: AgentRunId,
    },
}

impl AgentRunRecordKind {
    pub(crate) fn node_type(&self) -> &'static str {
        match self {
            Self::Root => "root_agent",
            Self::WorkflowTask { role, .. } => workflow_node_type(*role),
            Self::Subagent { .. } => "subagent",
            Self::Advisor { .. } => "advisor",
        }
    }

    pub(crate) fn extend_payload(&self, payload: &mut JsonObject) {
        match self {
            Self::WorkflowTask {
                workflow_id,
                iteration_id,
                attempt_id,
                role,
            } => {
                payload.insert("workflow_id".to_owned(), json!(workflow_id.as_str()));
                payload.insert("iteration_id".to_owned(), json!(iteration_id.as_str()));
                payload.insert("attempt_id".to_owned(), json!(attempt_id.as_str()));
                payload.insert("role".to_owned(), json!(role.as_str()));
            }
            Self::Subagent {
                parent_agent_run_id,
            }
            | Self::Advisor {
                parent_agent_run_id,
            } => {
                payload.insert(
                    "parent_agent_run_id".to_owned(),
                    json!(parent_agent_run_id.as_str()),
                );
            }
            Self::Root => {}
        }
    }
}

fn workflow_node_type(role: WorkflowTaskRole) -> &'static str {
    match role {
        WorkflowTaskRole::Planner => "workflow_planner",
        WorkflowTaskRole::Generator => "workflow_generator",
        WorkflowTaskRole::Reducer => "workflow_reducer",
    }
}
