use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use eos_types::JsonObject;
use eos_types::{ContentBlock, Message};
use serde_json::{json, Value};

use super::error::Result;
use super::io::{append_event, append_initial_message_rows, append_message_rows};
use super::record::MessageAppendRange;

/// A started agent-run message-record node.
#[derive(Debug, Clone)]
pub struct AgentRunRecordHandle {
    node_dir: PathBuf,
    messages_path: PathBuf,
    events_path: PathBuf,
}

impl AgentRunRecordHandle {
    /// Node directory.
    #[must_use]
    pub fn node_dir(&self) -> &Path {
        &self.node_dir
    }

    /// Append later model-visible messages and announce the byte range in
    /// `events.jsonl`.
    ///
    /// # Errors
    /// Returns [`super::MessageRecordError`] if message or event append fails.
    pub async fn append_messages(&self, messages: &[Message]) -> Result<MessageAppendRange> {
        let range = append_message_rows(&self.messages_path, "message", messages).await?;
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
            self.append_event("messages_appended", payload).await?;
        }
        Ok(range)
    }

    /// Append the terminal node event.
    ///
    /// # Errors
    /// Returns [`super::MessageRecordError`] if event append fails.
    pub async fn finish(&self, status: NodeFinishStatus) -> Result<()> {
        let mut payload = JsonObject::new();
        payload.insert("status".to_owned(), json!(status.as_str()));
        self.append_event("node_finished", payload).await
    }

    pub(crate) async fn append_initial_messages(
        &self,
        system_prompt: &str,
        initial_messages: &[Message],
    ) -> Result<MessageAppendRange> {
        append_initial_message_rows(&self.messages_path, system_prompt, initial_messages).await
    }

    pub(crate) async fn append_event(
        &self,
        kind: impl Into<String>,
        payload: JsonObject,
    ) -> Result<()> {
        append_event(&self.events_path, kind.into(), payload).await
    }

    pub(crate) fn from_node_dir(node_dir: PathBuf) -> Self {
        Self {
            messages_path: node_dir.join("messages.jsonl"),
            events_path: node_dir.join("events.jsonl"),
            node_dir,
        }
    }
}

/// Terminal status stored in `node_finished`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeFinishStatus {
    /// Agent run completed without framework error.
    Completed,
    /// Agent run failed or crashed.
    Failed,
}

impl NodeFinishStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
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
