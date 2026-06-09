//! Private adapter to engine-owned message records.

use eos_types::{AgentRunMessageRecordKind, WorkflowTaskRole};

/// Convert the public runner/port record kind into the engine message-record
/// type.
#[must_use]
pub fn to_message_record_kind(
    kind: &AgentRunMessageRecordKind,
) -> eos_engine::records::AgentRunRecordKind {
    match kind {
        AgentRunMessageRecordKind::Root => eos_engine::records::AgentRunRecordKind::Root,
        AgentRunMessageRecordKind::WorkflowTask {
            workflow_id,
            iteration_id,
            attempt_id,
            role,
        } => eos_engine::records::AgentRunRecordKind::WorkflowTask {
            workflow_id: workflow_id.clone(),
            iteration_id: iteration_id.clone(),
            attempt_id: attempt_id.clone(),
            role: to_message_record_workflow_role(*role),
        },
        AgentRunMessageRecordKind::Subagent {
            parent_agent_run_id,
        } => eos_engine::records::AgentRunRecordKind::Subagent {
            parent_agent_run_id: parent_agent_run_id.clone(),
        },
        AgentRunMessageRecordKind::Advisor {
            parent_agent_run_id,
        } => eos_engine::records::AgentRunRecordKind::Advisor {
            parent_agent_run_id: parent_agent_run_id.clone(),
        },
        AgentRunMessageRecordKind::Agent => eos_engine::records::AgentRunRecordKind::Agent,
        _ => eos_engine::records::AgentRunRecordKind::Agent,
    }
}

fn to_message_record_workflow_role(
    role: WorkflowTaskRole,
) -> eos_engine::records::WorkflowTaskRole {
    match role {
        WorkflowTaskRole::Planner => eos_engine::records::WorkflowTaskRole::Planner,
        WorkflowTaskRole::Generator => eos_engine::records::WorkflowTaskRole::Generator,
        WorkflowTaskRole::Reducer => eos_engine::records::WorkflowTaskRole::Reducer,
        _ => eos_engine::records::WorkflowTaskRole::Generator,
    }
}
