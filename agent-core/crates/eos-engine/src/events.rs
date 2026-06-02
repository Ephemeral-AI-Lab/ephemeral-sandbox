//! Engine-owned stream events.

use eos_llm_client::{Message, StopReason, UsageSnapshot};
use eos_types::{AgentRunId, JsonObject, ToolUseId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Payload carried by [`StreamEvent::AssistantMessageComplete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMessageComplete {
    /// The full assistant message.
    pub message: Message,
    /// Provider token usage for the turn.
    pub usage: UsageSnapshot,
    /// Provider stop reason, when reported.
    pub stop_reason: Option<StopReason>,
}

/// A broad agent-run stream event: provider deltas plus engine-domain tool,
/// background, and notification events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamEvent {
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
    /// Engine-dispatched background task started.
    BackgroundTaskStarted {
        /// Agent label; filled by [`stamp_identity`] when empty.
        #[serde(default)]
        agent_name: String,
        /// Agent run id; filled by [`stamp_identity`] when absent.
        #[serde(default)]
        agent_run_id: Option<AgentRunId>,
        /// Supervisor-local background task id.
        task_id: String,
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: JsonObject,
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

impl JsonSchema for StreamEvent {
    fn schema_name() -> String {
        "StreamEvent".to_owned()
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        <serde_json::Value>::json_schema(gen)
    }
}

/// Fill missing event identity from the query context identity.
#[must_use]
pub fn stamp_identity(
    mut event: StreamEvent,
    agent_name: &str,
    agent_run_id: &AgentRunId,
) -> StreamEvent {
    let fill = |name: &mut String, run_id: &mut Option<AgentRunId>| {
        if name.is_empty() {
            *name = agent_name.to_owned();
        }
        if run_id.is_none() {
            *run_id = Some(agent_run_id.clone());
        }
    };
    match &mut event {
        StreamEvent::ReasoningDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::AssistantTextDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::AssistantMessageComplete {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::ToolUseDelta {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::ToolExecutionStarted {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::ToolExecutionCompleted {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::ToolExecutionProgress {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::ToolExecutionCancelled {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::BackgroundTaskStarted {
            agent_name,
            agent_run_id,
            ..
        }
        | StreamEvent::SystemNotification {
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
        let event = StreamEvent::ToolExecutionProgress {
            agent_name: String::new(),
            agent_run_id: None,
            tool_use_id,
            tool_name: "read_file".to_owned(),
            output: "started".to_owned(),
        };

        let stamped = stamp_identity(event, "root", &run_id);
        match stamped {
            StreamEvent::ToolExecutionProgress {
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
