//! Private adapter from task-agent-run kinds to record kinds.

use eos_types::{ParentedAgentRunKind, TaskAgentRunKind};

use crate::records::AgentRunRecordKind;

/// Convert the task-facing run kind into the record kind.
#[must_use]
pub(crate) fn to_agent_run_record_kind(kind: &TaskAgentRunKind) -> AgentRunRecordKind {
    match kind {
        TaskAgentRunKind::Root => AgentRunRecordKind::Root,
        TaskAgentRunKind::Workflow { workflow, role } => AgentRunRecordKind::WorkflowTask {
            workflow_id: workflow.workflow_id.clone(),
            iteration_id: workflow.iteration_id.clone(),
            attempt_id: workflow.attempt_id.clone(),
            role: *role,
        },
        TaskAgentRunKind::Parented {
            parent_agent_run_id,
            kind,
        } => match kind {
            ParentedAgentRunKind::Subagent => AgentRunRecordKind::Subagent {
                parent_agent_run_id: parent_agent_run_id.clone(),
            },
            ParentedAgentRunKind::Advisor => AgentRunRecordKind::Advisor {
                parent_agent_run_id: parent_agent_run_id.clone(),
            },
        },
    }
}
