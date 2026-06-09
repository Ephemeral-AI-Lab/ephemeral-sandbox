use std::sync::Arc;

use eos_llm_client::{Message, StopReason, UsageSnapshot};
use eos_types::{AgentRunId, JsonObject, StartAgentLoopRequest, ToolUseId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::AgentRunRecordStore;

/// Payload carried by [`AgentRunStreamEvent::AssistantMessageComplete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMessageComplete {
    /// The full assistant message.
    pub message: Message,
    /// Provider token usage for the turn.
    pub usage: UsageSnapshot,
    /// Provider stop reason, when reported.
    pub stop_reason: Option<StopReason>,
}

/// A broad agent-run stream event: provider deltas plus engine-domain tool and
/// notification events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunStreamEvent {
    /// Incremental reasoning.
    ReasoningDelta {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Text fragment.
        text: String,
    },
    /// Incremental assistant text.
    AssistantTextDelta {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Text fragment.
        text: String,
    },
    /// Completed assistant message.
    AssistantMessageComplete {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Completion payload.
        payload: Box<AssistantMessageComplete>,
    },
    /// Fully assembled tool call emitted by the provider stream.
    ToolUseDelta {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Provider tool-use id.
        tool_use_id: ToolUseId,
        /// Tool name.
        name: String,
        /// Tool input.
        input: JsonObject,
    },
    /// Tool execution started.
    ToolExecutionStarted {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: JsonObject,
        /// Provider tool-use id.
        tool_use_id: ToolUseId,
    },
    /// Tool execution completed.
    ToolExecutionCompleted {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Tool name.
        tool_name: String,
        /// Tool output.
        output: String,
        /// Whether the result is an in-band tool error.
        is_error: bool,
        /// Provider tool-use id.
        tool_use_id: ToolUseId,
        /// Tool metadata.
        metadata: JsonObject,
        /// Whether this successful tool result ended the run.
        is_terminal: bool,
    },
    /// Tool execution progress.
    ToolExecutionProgress {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Provider tool-use id.
        tool_use_id: ToolUseId,
        /// Tool name.
        tool_name: String,
        /// Progress line.
        output: String,
    },
    /// Tool execution cancelled.
    ToolExecutionCancelled {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Provider tool-use id.
        tool_use_id: ToolUseId,
        /// Tool name.
        tool_name: String,
        /// Cancellation reason.
        reason: String,
    },
    /// Engine/system notification.
    SystemNotification {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Notification text.
        text: String,
    },
}

impl JsonSchema for AgentRunStreamEvent {
    fn schema_name() -> String {
        "AgentRunStreamEvent".to_owned()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        <serde_json::Value>::json_schema(gen)
    }
}

/// Per-run stream/tool/system event sink.
pub type AgentRunStreamSink = Arc<dyn Fn(&AgentRunStreamEvent) + Send + Sync>;

/// Factory for a live stream sink bound to one loop start request.
pub type AgentRunStreamSinkFactory =
    Arc<dyn Fn(&StartAgentLoopRequest) -> Option<AgentRunStreamSink> + Send + Sync>;

/// Output aggregate for live stream observations and durable run records.
#[derive(Clone, Default)]
pub struct AgentRunOutputs {
    stream: Option<AgentRunStreamSink>,
    record: Option<AgentRunRecordStore>,
}

impl std::fmt::Debug for AgentRunOutputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunOutputs")
            .field("has_stream", &self.stream.is_some())
            .field("has_record", &self.record.is_some())
            .finish()
    }
}

impl AgentRunOutputs {
    /// Create an empty output aggregate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach live stream observation.
    #[must_use]
    pub fn with_stream(mut self, stream: Option<AgentRunStreamSink>) -> Self {
        self.stream = stream;
        self
    }

    /// Attach durable agent-run record writing.
    #[must_use]
    pub fn with_record(mut self, record: Option<AgentRunRecordStore>) -> Self {
        self.record = record;
        self
    }

    pub(crate) fn observe(&self, event: &AgentRunStreamEvent) {
        if let Some(stream) = &self.stream {
            stream(event);
        }
    }

    pub(crate) fn record_store(&self) -> Option<&AgentRunRecordStore> {
        self.record.as_ref()
    }
}

/// Fill missing event identity from the query context identity.
#[must_use]
pub fn stamp_identity(
    mut event: AgentRunStreamEvent,
    agent_name: &str,
    agent_run_id: &AgentRunId,
) -> AgentRunStreamEvent {
    let fill = |name: &mut String, run_id: &mut Option<AgentRunId>| {
        if name.is_empty() {
            *name = agent_name.to_owned();
        }
        if run_id.is_none() {
            *run_id = Some(agent_run_id.clone());
        }
    };
    match &mut event {
        AgentRunStreamEvent::ReasoningDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::AssistantTextDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::AssistantMessageComplete {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::ToolUseDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::ToolExecutionStarted {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::ToolExecutionCompleted {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::ToolExecutionProgress {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::ToolExecutionCancelled {
            agent_name,
            agent_run_id,
            ..
        }
        | AgentRunStreamEvent::SystemNotification {
            agent_name,
            agent_run_id,
            ..
        } => fill(agent_name, agent_run_id),
    }
    event
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn stamp_identity_fills_empty_fields() {
        let run_id: AgentRunId = "run-1".parse().expect("valid id");
        let tool_use_id: ToolUseId = "toolu-1".parse().expect("valid tool id");
        let event = AgentRunStreamEvent::ToolExecutionProgress {
            agent_name: String::new(),
            agent_run_id: None,
            tool_use_id,
            tool_name: "read_file".to_owned(),
            output: "started".to_owned(),
        };

        let stamped = stamp_identity(event, "root", &run_id);
        match stamped {
            AgentRunStreamEvent::ToolExecutionProgress {
                agent_name,
                agent_run_id,
                ..
            } => {
                assert_eq!(agent_name, "root");
                assert_eq!(agent_run_id.as_ref(), Some(&run_id));
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
}
