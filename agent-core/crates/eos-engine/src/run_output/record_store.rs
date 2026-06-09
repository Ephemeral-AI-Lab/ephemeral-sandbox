use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use eos_types::{
    AgentRunRecordDir, AgentRunRecordTarget, ContentBlock, JsonObject, Message, MessageRole,
    ParentedAgentRunKind, TaskAgentRunKind, UtcDateTime, WorkflowTaskRole,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::error::{AgentRunRecordError, Result};
use super::layout;

/// File-backed store for resolved agent-run record trees.
#[derive(Debug, Clone)]
pub struct AgentRunRecordStore {
    root: PathBuf,
}

impl AgentRunRecordStore {
    /// Create a store rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create one agent-run record directory, write its initial messages, and
    /// append the initial record-local events.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError`] if path validation, directory
    /// creation, JSON encoding, or file append fails.
    pub async fn start_agent_run_at(
        &self,
        record_target: &AgentRunRecordTarget,
        agent_name: &str,
        system_prompt: &str,
        initial_messages: &[Message],
    ) -> Result<AgentRunRecordHandle> {
        validate_record_target(record_target)?;
        let record_dir = layout::record_dir(&self.root, &record_target.record_dir)?;
        tokio::fs::create_dir_all(&record_dir).await?;

        let mut handle = AgentRunRecordHandle::from_record_dir(
            record_dir,
            AgentRunRecordIdentity {
                request_id: record_target.request_id.to_string(),
                task_id: record_target.task_id.to_string(),
                agent_run_id: record_target.agent_run_id.to_string(),
            },
        );

        let mut payload = JsonObject::new();
        payload.insert(
            "type".to_owned(),
            json!(node_type(&record_target.task_agent_run_kind)),
        );
        payload.insert(
            "agent_run_id".to_owned(),
            json!(record_target.agent_run_id.as_str()),
        );
        payload.insert("agent".to_owned(), json!(agent_name));
        payload.insert("task_id".to_owned(), json!(record_target.task_id.as_str()));
        payload.insert(
            "request_id".to_owned(),
            json!(record_target.request_id.as_str()),
        );
        extend_payload(&record_target.task_agent_run_kind, &mut payload);
        handle.append_record_event("node_started", payload).await?;

        let range = handle
            .append_initial_messages(system_prompt, initial_messages)
            .await?;
        handle.initial_message_count = range.count;

        let mut payload = JsonObject::new();
        payload.insert("count".to_owned(), json!(range.count));
        payload.insert("messages_start_byte".to_owned(), json!(range.start_byte));
        payload.insert("messages_end_byte".to_owned(), json!(range.end_byte));
        handle
            .append_record_event("messages_initialized", payload)
            .await?;

        Ok(handle)
    }

    /// Read raw `messages.jsonl` bytes for a resolved record directory after
    /// `after_byte`.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError::NotFound`] if the record directory
    /// or message file does not exist.
    pub async fn read_messages_at(
        &self,
        record_dir: &AgentRunRecordDir,
        after_byte: u64,
    ) -> Result<MessageBytes> {
        let record_dir = layout::record_dir(&self.root, record_dir)?;
        read_bytes_after(&record_dir.join("messages.jsonl"), after_byte).await
    }

    /// Replay record-local events for a resolved record directory with
    /// `seq > after_seq`.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError::NotFound`] if the record directory
    /// or event file does not exist.
    pub async fn read_record_events_at(
        &self,
        record_dir: &AgentRunRecordDir,
        after_seq: u64,
    ) -> Result<Vec<AgentRunRecordEvent>> {
        let record_dir = layout::record_dir(&self.root, record_dir)?;
        read_record_events_after(&record_dir.join("events.jsonl"), after_seq).await
    }
}

/// A started agent-run record directory.
#[derive(Debug, Clone)]
pub struct AgentRunRecordHandle {
    record_dir: PathBuf,
    messages_path: PathBuf,
    events_path: PathBuf,
    identity: AgentRunRecordIdentity,
    initial_message_count: usize,
}

impl AgentRunRecordHandle {
    /// Record directory.
    #[must_use]
    pub fn record_dir(&self) -> &Path {
        &self.record_dir
    }

    /// Number of initial message rows written when the run was started.
    #[must_use]
    pub fn initial_message_count(&self) -> usize {
        self.initial_message_count
    }

    /// Append later model-visible messages and announce the byte range in
    /// `events.jsonl`.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError`] if message or event append fails.
    pub async fn append_messages(&self, messages: &[Message]) -> Result<MessageAppendRange> {
        let range =
            append_message_rows(&self.messages_path, &self.identity, "message", messages).await?;
        if range.count > 0 {
            let mut payload = JsonObject::new();
            payload.insert("count".to_owned(), json!(range.count));
            payload.insert("messages_start_byte".to_owned(), json!(range.start_byte));
            payload.insert("messages_end_byte".to_owned(), json!(range.end_byte));
            payload.insert(
                "message_types".to_owned(),
                Value::Array(
                    message_types(messages)
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            self.append_record_event("messages_appended", payload)
                .await?;
        }
        Ok(range)
    }

    /// Read raw `messages.jsonl` bytes after `after_byte`.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError`] if the message file is missing or
    /// the byte offset is out of range.
    pub async fn read_messages(&self, after_byte: u64) -> Result<MessageBytes> {
        read_bytes_after(&self.messages_path, after_byte).await
    }

    /// Replay record-local events with `seq > after_seq`.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError`] if the event file is missing or an
    /// event row cannot be decoded.
    pub async fn read_record_events(&self, after_seq: u64) -> Result<Vec<AgentRunRecordEvent>> {
        read_record_events_after(&self.events_path, after_seq).await
    }

    /// Append the terminal record event.
    ///
    /// # Errors
    /// Returns [`super::AgentRunRecordError`] if event append fails.
    pub async fn finish(&self, status: AgentRunRecordFinishStatus) -> Result<()> {
        let mut payload = JsonObject::new();
        payload.insert("status".to_owned(), json!(status.as_str()));
        self.append_record_event("node_finished", payload).await
    }

    async fn append_record_event(
        &self,
        kind: impl Into<String>,
        payload: JsonObject,
    ) -> Result<()> {
        append_record_event(&self.events_path, &self.identity, kind.into(), payload).await
    }

    async fn append_initial_messages(
        &self,
        system_prompt: &str,
        initial_messages: &[Message],
    ) -> Result<MessageAppendRange> {
        append_initial_message_rows(
            &self.messages_path,
            &self.identity,
            system_prompt,
            initial_messages,
        )
        .await
    }

    fn from_record_dir(record_dir: PathBuf, identity: AgentRunRecordIdentity) -> Self {
        Self {
            messages_path: record_dir.join("messages.jsonl"),
            events_path: record_dir.join("events.jsonl"),
            record_dir,
            identity,
            initial_message_count: 0,
        }
    }
}

/// Terminal status stored in `node_finished`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunRecordFinishStatus {
    /// Agent run completed without framework error.
    Completed,
    /// Agent run failed or crashed.
    Failed,
}

impl AgentRunRecordFinishStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Stable identity columns written on every record row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRecordIdentity {
    /// Owning request id.
    pub request_id: String,
    /// Owning task id.
    pub task_id: String,
    /// Agent-run id.
    pub agent_run_id: String,
}

/// Byte range produced by a message append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageAppendRange {
    /// Number of message rows appended.
    pub count: usize,
    /// Starting byte offset before the append.
    pub start_byte: u64,
    /// Ending byte offset after the append.
    pub end_byte: u64,
}

/// Raw message-record bytes plus the next tail offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBytes {
    /// Raw JSONL bytes.
    pub bytes: Vec<u8>,
    /// Byte offset after `bytes`.
    pub next_byte_offset: u64,
}

/// One record-local `events.jsonl` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunRecordEvent {
    /// Owning request id.
    #[serde(default)]
    pub request_id: String,
    /// Owning task id.
    #[serde(default)]
    pub task_id: String,
    /// Agent-run id.
    #[serde(default)]
    pub agent_run_id: String,
    /// Record-local sequence, starting at 1.
    pub seq: u64,
    /// Stable event category.
    pub kind: String,
    /// Small routing/status payload.
    pub payload: JsonObject,
    /// Event creation timestamp.
    pub created_at: UtcDateTime,
}

#[derive(Serialize)]
struct MessageRow<'a> {
    #[serde(rename = "type")]
    row_type: &'static str,
    request_id: &'a str,
    task_id: &'a str,
    agent_run_id: &'a str,
    role: &'static str,
    content: &'a [ContentBlock],
}

#[derive(Serialize)]
struct MessageRowOwned<'a> {
    #[serde(rename = "type")]
    row_type: &'static str,
    request_id: &'a str,
    task_id: &'a str,
    agent_run_id: &'a str,
    role: &'static str,
    content: Vec<ContentBlock>,
}

async fn append_message_rows(
    path: &Path,
    identity: &AgentRunRecordIdentity,
    row_type: &'static str,
    messages: &[Message],
) -> Result<MessageAppendRange> {
    let rows: Vec<_> = messages
        .iter()
        .map(|message| MessageRow {
            row_type,
            request_id: identity.request_id.as_str(),
            task_id: identity.task_id.as_str(),
            agent_run_id: identity.agent_run_id.as_str(),
            role: role_wire(message.role),
            content: &message.content,
        })
        .collect();
    append_rows(path, &rows).await
}

async fn append_initial_message_rows(
    path: &Path,
    identity: &AgentRunRecordIdentity,
    system_prompt: &str,
    initial_messages: &[Message],
) -> Result<MessageAppendRange> {
    let mut rows = Vec::with_capacity(initial_messages.len().saturating_add(1));
    rows.push(MessageRowOwned {
        row_type: "initial_message",
        request_id: identity.request_id.as_str(),
        task_id: identity.task_id.as_str(),
        agent_run_id: identity.agent_run_id.as_str(),
        role: "system",
        content: vec![ContentBlock::Text {
            text: system_prompt.to_owned(),
        }],
    });
    rows.extend(initial_messages.iter().map(|message| MessageRowOwned {
        row_type: "initial_message",
        request_id: identity.request_id.as_str(),
        task_id: identity.task_id.as_str(),
        agent_run_id: identity.agent_run_id.as_str(),
        role: role_wire(message.role),
        content: message.content.clone(),
    }));
    append_rows(path, &rows).await
}

async fn append_rows<T: Serialize>(path: &Path, rows: &[T]) -> Result<MessageAppendRange> {
    let start_byte = file_len_or_zero(path).await?;
    if rows.is_empty() {
        return Ok(MessageAppendRange {
            count: 0,
            start_byte,
            end_byte: start_byte,
        });
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    for row in rows {
        let line = serde_json::to_string(row)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }
    file.flush().await?;
    let end_byte = file_len_or_zero(path).await?;
    Ok(MessageAppendRange {
        count: rows.len(),
        start_byte,
        end_byte,
    })
}

async fn append_record_event(
    path: &Path,
    identity: &AgentRunRecordIdentity,
    kind: String,
    payload: JsonObject,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let seq = next_record_event_seq(path).await?;
    let event = AgentRunRecordEvent {
        request_id: identity.request_id.clone(),
        task_id: identity.task_id.clone(),
        agent_run_id: identity.agent_run_id.clone(),
        seq,
        kind,
        payload,
        created_at: UtcDateTime::now(),
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let line = serde_json::to_string(&event)?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

async fn next_record_event_seq(path: &Path) -> Result<u64> {
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => {
            let last_seq = raw
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(serde_json::from_str::<AgentRunRecordEvent>)
                .transpose()?
                .map_or(0, |event| event.seq);
            Ok(last_seq.saturating_add(1))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(1),
        Err(err) => Err(err.into()),
    }
}

async fn read_bytes_after(path: &Path, after_byte: u64) -> Result<MessageBytes> {
    let mut file = tokio::fs::File::open(path).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AgentRunRecordError::missing_path(path)
        } else {
            AgentRunRecordError::Io(err)
        }
    })?;
    let len = file.metadata().await?.len();
    if after_byte > len {
        return Err(AgentRunRecordError::OffsetOutOfRange {
            offset: after_byte,
            len,
        });
    }
    file.seek(std::io::SeekFrom::Start(after_byte)).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    Ok(MessageBytes {
        bytes,
        next_byte_offset: len,
    })
}

async fn read_record_events_after(path: &Path, after_seq: u64) -> Result<Vec<AgentRunRecordEvent>> {
    let raw = tokio::fs::read_to_string(path).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AgentRunRecordError::missing_path(path)
        } else {
            AgentRunRecordError::Io(err)
        }
    })?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<AgentRunRecordEvent>)
        .filter_map(|result| match result {
            Ok(event) if event.seq > after_seq => Some(Ok(event)),
            Ok(_) => None,
            Err(err) => Some(Err(AgentRunRecordError::Json(err))),
        })
        .collect()
}

async fn file_len_or_zero(path: &Path) -> Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

fn validate_record_target(target: &AgentRunRecordTarget) -> Result<()> {
    safe_segment("request_id", target.request_id.as_str())?;
    safe_segment("task_id", target.task_id.as_str())?;
    safe_segment("agent-run", target.agent_run_id.as_str())?;
    validate_kind_segments(&target.task_agent_run_kind)
}

fn validate_kind_segments(kind: &TaskAgentRunKind) -> Result<()> {
    match kind {
        TaskAgentRunKind::Workflow { workflow, .. } => {
            safe_segment("workflow", workflow.workflow_id.as_str())?;
            safe_segment("iteration", workflow.iteration_id.as_str())?;
            safe_segment("attempt", workflow.attempt_id.as_str())?;
            Ok(())
        }
        TaskAgentRunKind::Parented {
            parent_agent_run_id,
            ..
        } => {
            safe_segment("agent_run_id", parent_agent_run_id.as_str())?;
            Ok(())
        }
        TaskAgentRunKind::Root => Ok(()),
    }
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

fn node_type(kind: &TaskAgentRunKind) -> &'static str {
    match kind {
        TaskAgentRunKind::Root => "root_agent",
        TaskAgentRunKind::Workflow { role, .. } => workflow_node_type(*role),
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Subagent,
            ..
        } => "subagent",
        TaskAgentRunKind::Parented {
            kind: ParentedAgentRunKind::Advisor,
            ..
        } => "advisor",
    }
}

fn extend_payload(kind: &TaskAgentRunKind, payload: &mut JsonObject) {
    match kind {
        TaskAgentRunKind::Workflow { workflow, role } => {
            payload.insert(
                "workflow_id".to_owned(),
                json!(workflow.workflow_id.as_str()),
            );
            payload.insert(
                "iteration_id".to_owned(),
                json!(workflow.iteration_id.as_str()),
            );
            payload.insert("attempt_id".to_owned(), json!(workflow.attempt_id.as_str()));
            payload.insert("role".to_owned(), json!(role.as_str()));
        }
        TaskAgentRunKind::Parented {
            parent_agent_run_id,
            ..
        } => {
            payload.insert(
                "parent_agent_run_id".to_owned(),
                json!(parent_agent_run_id.as_str()),
            );
        }
        TaskAgentRunKind::Root => {}
    }
}

fn workflow_node_type(role: WorkflowTaskRole) -> &'static str {
    match role {
        WorkflowTaskRole::Planner => "workflow_planner",
        WorkflowTaskRole::Generator => "workflow_generator",
        WorkflowTaskRole::Reducer => "workflow_reducer",
    }
}

fn message_types(messages: &[Message]) -> Vec<String> {
    let mut types = BTreeSet::new();
    for block in messages.iter().flat_map(|message| &message.content) {
        types.insert(
            match block {
                ContentBlock::Text { .. } => "text",
                ContentBlock::ToolUse { .. } => "tool_use",
                ContentBlock::Reasoning { .. } => "reasoning",
                ContentBlock::ToolResult { .. } => "tool_result",
                ContentBlock::SystemNotification { .. } => "system_notification",
                _ => "unknown",
            }
            .to_owned(),
        );
    }
    types.into_iter().collect()
}

fn role_wire(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
