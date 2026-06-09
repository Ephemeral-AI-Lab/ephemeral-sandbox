//! Task routes: the per-request task tree, task detail, and the task transcript.
//! Request task lists flow through `AgentCoreService`; task detail and transcript
//! routes use store/record handles kept in backend API state.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use eos_engine::records::AgentRunRecordError;
use eos_types::{format_record_dir, AgentRun, Task, TaskRun};
use eos_types::{AgentRunId, TaskId};

use super::parse_id;
use crate::error::ApiError;
use crate::router::AppState;

/// `GET /api/agent-core/requests/{request_id}/tasks` — task-agent-runs for one
/// request.
pub async fn request_tasks(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Result<Json<Vec<TaskRun>>, ApiError> {
    let request_id = parse_id(&request_id, "request")?;
    Ok(Json(
        state
            .agent_core
            .list_user_request_tasks(&request_id)
            .await?,
    ))
}

/// Task detail: the persisted task joined with its latest agent run, if any.
#[derive(Debug, Serialize)]
pub struct TaskDetail {
    /// The persisted task row.
    pub task: Task,
    /// The task's latest agent run, when one exists.
    pub agent_run: Option<AgentRun>,
}

/// `GET /api/agent-core/tasks/{task_id}` — task detail plus its related agent run.
pub async fn detail(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TaskDetail>, ApiError> {
    let task_id: TaskId = parse_id(&task_id, "task")?;
    let task = state
        .task_store
        .get(&task_id)
        .await?
        .ok_or(ApiError::NotFound("task"))?;
    let agent_run = state.agent_run_store.get_for_task(&task_id).await?;
    Ok(Json(TaskDetail { task, agent_run }))
}

/// The model/tool transcript for a task.
#[derive(Debug, Serialize)]
pub struct TranscriptResponse {
    /// The task the transcript belongs to.
    pub task_id: TaskId,
    /// The agent run that produced the transcript, when one exists.
    pub agent_run_id: Option<AgentRunId>,
    /// The transcript messages (provider-neutral JSON blocks).
    pub messages: Vec<serde_json::Value>,
}

/// `GET /api/agent-core/tasks/{task_id}/transcript` — the task's model/tool
/// transcript, drawn from its agent run's message history.
pub async fn transcript(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<TranscriptResponse>, ApiError> {
    let task_id: TaskId = parse_id(&task_id, "task")?;
    if state.task_store.get(&task_id).await?.is_none() {
        return Err(ApiError::NotFound("task"));
    }
    let run = state.agent_run_store.get_for_task(&task_id).await?;
    let (agent_run_id, messages) = match run {
        Some(run) => {
            let record_dir = state
                .task_agent_run_store
                .record_index_for_agent_run(&run.id)
                .await?
                .map(|index| format_record_dir(&index))
                .ok_or(ApiError::NotFound("agent run"))?;
            let messages = match state.message_records.read_messages_at(&record_dir, 0).await {
                Ok(bytes) => parse_jsonl_messages(&bytes.bytes)?,
                Err(AgentRunRecordError::NotFound(_)) => Vec::new(),
                Err(err) => return Err(ApiError::from(err)),
            };
            (Some(run.id), messages)
        }
        None => (None, Vec::new()),
    };
    Ok(Json(TranscriptResponse {
        task_id,
        agent_run_id,
        messages,
    }))
}

fn parse_jsonl_messages(bytes: &[u8]) -> Result<Vec<serde_json::Value>, ApiError> {
    let text = std::str::from_utf8(bytes).map_err(|err| {
        tracing::error!(error = %err, "agent transcript message record was not utf8");
        ApiError::Internal
    })?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|err| {
                tracing::error!(error = %err, "agent transcript message record row was invalid json");
                ApiError::Internal
            })
        })
        .collect()
}
