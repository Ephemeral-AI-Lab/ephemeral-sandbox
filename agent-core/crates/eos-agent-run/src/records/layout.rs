use std::path::{Path, PathBuf};

use eos_types::AgentRunId;

use super::error::{MessageRecordError, Result};
use super::kind::{AgentRunRecordKind, AgentRunRecordStart};

pub(crate) async fn resolve_agent_run(root: &Path, agent_run_id: &AgentRunId) -> Result<PathBuf> {
    safe_segment("agent_run_id", agent_run_id.as_str())?;
    let root = root.to_path_buf();
    let id = agent_run_id.clone();
    let found = tokio::task::spawn_blocking(move || find_agent_run_dir_in(&root, &id)).await??;
    found.ok_or_else(|| MessageRecordError::NotFound(agent_run_id.as_str().to_owned()))
}

pub(crate) fn node_dir(root: &Path, input: &AgentRunRecordStart<'_>) -> Result<PathBuf> {
    let request_root = request_root(root, input.request_id.as_str())?;
    let agent_run_segment = safe_prefixed_segment("agent-run", input.agent_run_id.as_str())?;
    match &input.kind {
        AgentRunRecordKind::Root => {
            let task_id = required_task_id(input)?;
            Ok(request_root
                .join(safe_prefixed_segment("root-task", task_id)?)
                .join(agent_run_segment))
        }
        AgentRunRecordKind::WorkflowTask {
            workflow_id,
            iteration_id,
            attempt_id,
            role,
        } => {
            let task_id = required_task_id(input)?;
            let workflow_parent =
                find_root_agent_dir(&request_root)?.unwrap_or_else(|| request_root.clone());
            Ok(workflow_parent
                .join("workflows")
                .join(safe_prefixed_segment("workflow", workflow_id.as_str())?)
                .join(safe_prefixed_segment("iteration", iteration_id.as_str())?)
                .join(safe_prefixed_segment("attempt", attempt_id.as_str())?)
                .join(safe_prefixed_segment(role.task_segment_prefix(), task_id)?)
                .join(agent_run_segment))
        }
        AgentRunRecordKind::Subagent {
            parent_agent_run_id,
        } => Ok(parent_or_request_dir(&request_root, parent_agent_run_id)?
            .join("subagents")
            .join(safe_prefixed_segment(
                "subagent-run",
                input.agent_run_id.as_str(),
            )?)),
        AgentRunRecordKind::Advisor {
            parent_agent_run_id,
        } => Ok(parent_or_request_dir(&request_root, parent_agent_run_id)?
            .join("advisors")
            .join(safe_prefixed_segment(
                "advisor-run",
                input.agent_run_id.as_str(),
            )?)),
    }
}

pub(crate) fn parent_announcement(
    root: &Path,
    input: &AgentRunRecordStart<'_>,
    node_dir: &Path,
) -> Result<Option<(PathBuf, String)>> {
    let request_root = request_root(root, input.request_id.as_str())?;
    let parent = match &input.kind {
        AgentRunRecordKind::Subagent {
            parent_agent_run_id,
        }
        | AgentRunRecordKind::Advisor {
            parent_agent_run_id,
        } => find_agent_run_dir_in(&request_root, parent_agent_run_id)?,
        AgentRunRecordKind::WorkflowTask { .. } => find_root_agent_dir(&request_root)?,
        AgentRunRecordKind::Root => None,
    };
    let Some(parent) = parent else {
        return Ok(None);
    };
    let relative = node_dir
        .strip_prefix(&parent)
        .ok()
        .map(path_to_slash_string)
        .unwrap_or_else(|| path_to_slash_string(node_dir));
    Ok(Some((parent, relative)))
}

fn request_root(root: &Path, request_id: &str) -> Result<PathBuf> {
    Ok(root
        .join("requests")
        .join(safe_segment("request_id", request_id)?))
}

fn required_task_id<'a>(input: &AgentRunRecordStart<'a>) -> Result<&'a str> {
    input
        .task_id
        .map(eos_types::TaskId::as_str)
        .ok_or_else(|| MessageRecordError::unsafe_segment("task_id", ""))
}

fn parent_or_request_dir(request_root: &Path, parent_agent_run_id: &AgentRunId) -> Result<PathBuf> {
    Ok(
        find_agent_run_dir_in(request_root, parent_agent_run_id)?.unwrap_or_else(|| {
            request_root
                .join("parents-missing")
                .join(parent_agent_run_id.as_str())
        }),
    )
}

fn find_root_agent_dir(request_root: &Path) -> Result<Option<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(request_root) else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("root-task-") {
            continue;
        }
        let Ok(agent_dirs) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for agent_entry in agent_dirs {
            let agent_entry = agent_entry?;
            if !agent_entry.file_type()?.is_dir() {
                continue;
            }
            if agent_entry
                .file_name()
                .to_str()
                .is_some_and(|value| value.starts_with("agent-run-"))
            {
                return Ok(Some(agent_entry.path()));
            }
        }
    }
    Ok(None)
}

fn find_agent_run_dir_in(root: &Path, agent_run_id: &AgentRunId) -> Result<Option<PathBuf>> {
    safe_segment("agent_run_id", agent_run_id.as_str())?;
    let needles = [
        safe_prefixed_segment("agent-run", agent_run_id.as_str())?,
        safe_prefixed_segment("subagent-run", agent_run_id.as_str())?,
        safe_prefixed_segment("advisor-run", agent_run_id.as_str())?,
    ];
    find_dir_named(root, &needles)
}

fn find_dir_named(root: &Path, needles: &[String]) -> Result<Option<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|value| needles.iter().any(|needle| needle == value))
        {
            return Ok(Some(entry.path()));
        }
        if let Some(found) = find_dir_named(&entry.path(), needles)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn safe_prefixed_segment(prefix: &'static str, id: &str) -> Result<String> {
    Ok(format!("{prefix}-{}", safe_segment(prefix, id)?))
}

fn safe_segment<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains(std::path::MAIN_SEPARATOR)
    {
        return Err(MessageRecordError::unsafe_segment(field, value));
    }
    Ok(value)
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
