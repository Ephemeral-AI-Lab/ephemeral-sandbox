use std::path::{Path, PathBuf};

use eos_types::{
    format_record_dir, AgentRunRecordDir, AgentRunRecordIndex, ParentedAgentRunKind,
    TaskAgentRunKind, WorkflowCoordinates,
};

use super::error::{AgentRunRecordError, Result};
use super::kind::{AgentRunRecordKind, AgentRunRecordStart};

pub(crate) fn node_dir(root: &Path, input: &AgentRunRecordStart<'_>) -> Result<PathBuf> {
    validate_start_segments(input)?;
    let task_id = input
        .task_id
        .cloned()
        .ok_or_else(|| AgentRunRecordError::unsafe_segment("task_id", ""))?;
    let kind = match input.kind {
        AgentRunRecordKind::Root => TaskAgentRunKind::Root,
        AgentRunRecordKind::WorkflowTask {
            workflow_id,
            iteration_id,
            attempt_id,
            role,
        } => TaskAgentRunKind::Workflow {
            workflow: WorkflowCoordinates {
                workflow_id: workflow_id.clone(),
                iteration_id: iteration_id.clone(),
                attempt_id: attempt_id.clone(),
            },
            role: *role,
        },
        AgentRunRecordKind::Subagent {
            parent_agent_run_id,
        } => TaskAgentRunKind::Parented {
            parent_agent_run_id: parent_agent_run_id.clone(),
            kind: ParentedAgentRunKind::Subagent,
        },
        AgentRunRecordKind::Advisor {
            parent_agent_run_id,
        } => TaskAgentRunKind::Parented {
            parent_agent_run_id: parent_agent_run_id.clone(),
            kind: ParentedAgentRunKind::Advisor,
        },
    };
    let record_dir = format_record_dir(&AgentRunRecordIndex {
        request_id: input.request_id.clone(),
        agent_run_id: input.agent_run_id.clone(),
        task_id,
        kind,
        parent_record_dir: None,
    });
    record_dir_path(root, record_dir.as_str())
}

pub(crate) fn record_dir(root: &Path, record_dir: &AgentRunRecordDir) -> Result<PathBuf> {
    record_dir_path(root, record_dir.as_str())
}

pub(crate) fn validate_start_segments(input: &AgentRunRecordStart<'_>) -> Result<()> {
    safe_segment("request_id", input.request_id.as_str())?;
    safe_segment("agent-run", input.agent_run_id.as_str())?;
    if let Some(task_id) = input.task_id {
        safe_segment("task_id", task_id.as_str())?;
    }
    match input.kind {
        AgentRunRecordKind::WorkflowTask {
            workflow_id,
            iteration_id,
            attempt_id,
            ..
        } => {
            safe_segment("workflow", workflow_id.as_str())?;
            safe_segment("iteration", iteration_id.as_str())?;
            safe_segment("attempt", attempt_id.as_str())?;
        }
        AgentRunRecordKind::Subagent {
            parent_agent_run_id,
        }
        | AgentRunRecordKind::Advisor {
            parent_agent_run_id,
        } => {
            safe_segment("agent_run_id", parent_agent_run_id.as_str())?;
        }
        AgentRunRecordKind::Root => {}
    }
    Ok(())
}

fn record_dir_path(root: &Path, record_dir: &str) -> Result<PathBuf> {
    let mut node = root.to_path_buf();
    for segment in record_dir.split('/') {
        node.push(safe_segment("record_dir", segment)?);
    }
    Ok(node)
}

fn safe_segment<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains(std::path::MAIN_SEPARATOR)
    {
        return Err(AgentRunRecordError::unsafe_segment(field, value));
    }
    Ok(value)
}
