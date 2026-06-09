use std::path::PathBuf;

use eos_types::{AgentRunRecordDir, JsonObject};
use serde_json::json;

use super::error::Result;
use super::handle::AgentRunRecordHandle;
use super::io::{read_bytes_after, read_events_after};
use super::kind::AgentRunRecordStart;
use super::layout;
use super::record::{NodeEvent, RecordBytes};

/// Shared message-record root service.
#[derive(Debug, Clone)]
pub struct AgentMessageRecords {
    root: PathBuf,
}

impl AgentMessageRecords {
    /// Create a service rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create one agent-run node, write its initial messages, and append the
    /// initial node-local events.
    ///
    /// # Errors
    /// Returns [`super::MessageRecordError`] if path validation, directory
    /// creation, JSON encoding, or file append fails.
    pub async fn start_agent_run(
        &self,
        input: AgentRunRecordStart<'_>,
    ) -> Result<AgentRunRecordHandle> {
        let node_dir = layout::node_dir(&self.root, &input)?;
        tokio::fs::create_dir_all(&node_dir).await?;

        let handle = AgentRunRecordHandle::from_node_dir(node_dir.clone());
        let mut payload = JsonObject::new();
        payload.insert("type".to_owned(), json!(input.kind.node_type()));
        payload.insert(
            "agent_run_id".to_owned(),
            json!(input.agent_run_id.as_str()),
        );
        payload.insert("agent".to_owned(), json!(input.agent_name));
        if let Some(task_id) = input.task_id {
            payload.insert("task_id".to_owned(), json!(task_id.as_str()));
        }
        payload.insert("request_id".to_owned(), json!(input.request_id.as_str()));
        input.kind.extend_payload(&mut payload);
        handle.append_event("node_started", payload).await?;

        let range = handle
            .append_initial_messages(input.system_prompt, input.initial_messages)
            .await?;
        let mut payload = JsonObject::new();
        payload.insert("count".to_owned(), json!(range.count));
        payload.insert("messages_start_byte".to_owned(), json!(range.start_byte));
        payload.insert("messages_end_byte".to_owned(), json!(range.end_byte));
        handle.append_event("messages_initialized", payload).await?;

        Ok(handle)
    }

    /// Read raw `messages.jsonl` bytes for a resolved record directory after
    /// `after_byte`.
    ///
    /// # Errors
    /// Returns [`super::MessageRecordError::NotFound`] if the node or message
    /// file does not exist.
    pub async fn read_messages_at(
        &self,
        record_dir: &AgentRunRecordDir,
        after_byte: u64,
    ) -> Result<RecordBytes> {
        let node_dir = layout::record_dir(&self.root, record_dir)?;
        read_bytes_after(&node_dir.join("messages.jsonl"), after_byte).await
    }

    /// Replay node-local events for a resolved record directory with
    /// `seq > after_seq`.
    ///
    /// # Errors
    /// Returns [`super::MessageRecordError::NotFound`] if the node or event file
    /// does not exist.
    pub async fn read_events_at(
        &self,
        record_dir: &AgentRunRecordDir,
        after_seq: u64,
    ) -> Result<Vec<NodeEvent>> {
        let node_dir = layout::record_dir(&self.root, record_dir)?;
        read_events_after(&node_dir.join("events.jsonl"), after_seq).await
    }
}
