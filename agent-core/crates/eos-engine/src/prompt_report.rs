//! Prompt-report JSONL writer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eos_llm_client::{Message, ToolSpec, UsageSnapshot};
use eos_types::AgentRunId;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::EngineError;

#[derive(Debug, Default)]
struct PromptReportState {
    next_seq: u64,
}

/// File-backed prompt-report recorder.
#[derive(Debug, Clone)]
pub struct PromptReportRecorder {
    path: PathBuf,
    agent_run_id: AgentRunId,
    agent: String,
    model: String,
    state: Arc<Mutex<PromptReportState>>,
}

#[derive(Serialize)]
struct BaseEvent<'a> {
    agent_run_id: &'a AgentRunId,
    agent: &'a str,
    model: &'a str,
}

#[derive(Serialize)]
struct LlmRequestEvent<'a> {
    #[serde(flatten)]
    base: BaseEvent<'a>,
    event: &'static str,
    seq: u64,
    system_prompt: &'a str,
    messages: &'a [Message],
    tools: &'a [ToolSpec],
}

#[derive(Serialize)]
struct AssistantEvent<'a> {
    #[serde(flatten)]
    base: BaseEvent<'a>,
    event: &'static str,
    seq: u64,
    message: &'a Message,
    usage: UsageSnapshot,
}

#[derive(Serialize)]
struct ToolResultsEvent<'a> {
    #[serde(flatten)]
    base: BaseEvent<'a>,
    event: &'static str,
    seq: u64,
    tool_results: &'a [eos_llm_client::ContentBlock],
}

impl PromptReportRecorder {
    /// Create a recorder writing append-only JSONL at `path`.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        agent_run_id: AgentRunId,
        agent: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            agent_run_id,
            agent: agent.into(),
            model: model.into(),
            state: Arc::new(Mutex::new(PromptReportState::default())),
        }
    }

    /// Recorder output path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the next turn sequence number.
    pub async fn next_seq(&self) -> u64 {
        let mut state = self.state.lock().await;
        let seq = state.next_seq;
        state.next_seq = state.next_seq.saturating_add(1);
        seq
    }

    fn base(&self) -> BaseEvent<'_> {
        BaseEvent {
            agent_run_id: &self.agent_run_id,
            agent: &self.agent,
            model: &self.model,
        }
    }

    async fn append_json<T: Serialize>(&self, value: &T) -> Result<(), EngineError> {
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        let line = serde_json::to_string(value)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }

    /// Record the model request row for a turn.
    ///
    /// # Errors
    /// Returns an error when the JSONL row cannot be written.
    pub async fn record_llm_request(
        &self,
        seq: u64,
        system_prompt: &str,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<(), EngineError> {
        self.append_json(&LlmRequestEvent {
            base: self.base(),
            event: "llm_request",
            seq,
            system_prompt,
            messages,
            tools,
        })
        .await
    }

    /// Record the assistant completion row for a turn.
    ///
    /// # Errors
    /// Returns an error when the JSONL row cannot be written.
    pub async fn record_assistant(
        &self,
        seq: u64,
        message: &Message,
        usage: UsageSnapshot,
    ) -> Result<(), EngineError> {
        self.append_json(&AssistantEvent {
            base: self.base(),
            event: "assistant",
            seq,
            message,
            usage,
        })
        .await
    }

    /// Record the tool-result row for a turn.
    ///
    /// # Errors
    /// Returns an error when the JSONL row cannot be written.
    pub async fn record_tool_results(
        &self,
        seq: u64,
        tool_results: &[eos_llm_client::ContentBlock],
    ) -> Result<(), EngineError> {
        self.append_json(&ToolResultsEvent {
            base: self.base(),
            event: "tool_results",
            seq,
            tool_results,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use eos_llm_client::{ContentBlock, Message, MessageRole, ToolSpec, UsageSnapshot};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn prompt_report_matches_golden() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("prompt.jsonl");
        let recorder = PromptReportRecorder::new(
            &path,
            "run-1".parse().expect("valid run id"),
            "root",
            "model-a",
        );
        let seq = recorder.next_seq().await;
        let messages = vec![Message::from_user_text("hello")];
        let tools = vec![ToolSpec::new(
            "read_file",
            "read",
            json!({"type":"object"})
                .as_object()
                .expect("object")
                .clone(),
            None,
        )];
        recorder
            .record_llm_request(seq, "system text", &messages, &tools)
            .await
            .expect("request report");
        let assistant = Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "ok".to_owned(),
            }],
        };
        recorder
            .record_assistant(
                seq,
                &assistant,
                UsageSnapshot {
                    input_tokens: 3,
                    output_tokens: 2,
                },
            )
            .await
            .expect("assistant report");
        recorder
            .record_tool_results(seq, &[])
            .await
            .expect("tool report");

        let raw = tokio::fs::read_to_string(path).await.expect("read report");
        let lines: Vec<serde_json::Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid json"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["event"], json!("llm_request"));
        assert_eq!(lines[1]["event"], json!("assistant"));
        assert_eq!(lines[2]["event"], json!("tool_results"));
        assert_eq!(lines[0]["seq"], json!(0));
        assert_eq!(lines[1]["seq"], json!(0));
        assert_eq!(lines[2]["seq"], json!(0));
        assert_eq!(lines[0]["system_prompt"], json!("system text"));
        assert!(lines[0]["messages"]
            .as_array()
            .expect("messages array")
            .iter()
            .all(|m| m["role"] != json!("system")));
    }

    #[test]
    fn no_system_role_in_transcript() {
        let message = Message::from_user_text("user");
        let value = serde_json::to_value(message).expect("serialize");
        assert_ne!(value["role"], json!("system"));
        assert!(serde_json::from_value::<MessageRole>(json!("system")).is_err());
    }
}
