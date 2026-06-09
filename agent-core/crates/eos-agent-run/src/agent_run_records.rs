//! Private adapter from task-agent-run kinds to record kinds.

use eos_types::{ParentedAgentRunKind, TaskAgentRunKind, WorkflowTaskRole};

use crate::records::{AgentRunRecordKind, WorkflowTaskRole as RecordWorkflowTaskRole};

/// Convert the task-facing run kind into the record kind.
#[must_use]
pub(crate) fn to_agent_run_record_kind(kind: &TaskAgentRunKind) -> AgentRunRecordKind {
    match kind {
        TaskAgentRunKind::Root => AgentRunRecordKind::Root,
        TaskAgentRunKind::Workflow { workflow, role } => AgentRunRecordKind::WorkflowTask {
            workflow_id: workflow.workflow_id.clone(),
            iteration_id: workflow.iteration_id.clone(),
            attempt_id: workflow.attempt_id.clone(),
            role: to_agent_run_record_workflow_role(*role),
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

fn to_agent_run_record_workflow_role(role: WorkflowTaskRole) -> RecordWorkflowTaskRole {
    match role {
        WorkflowTaskRole::Planner => RecordWorkflowTaskRole::Planner,
        WorkflowTaskRole::Generator => RecordWorkflowTaskRole::Generator,
        WorkflowTaskRole::Reducer => RecordWorkflowTaskRole::Reducer,
    }
}
