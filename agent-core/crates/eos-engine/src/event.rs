//! Engine event data, observation, and rendering.

// Phase 04 intentionally keeps event data at `event/event.rs` so data,
// observation, and rendering stay as sibling files under the event owner.
#[allow(clippy::module_inception)]
mod event {
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

    /// A broad agent-run stream event: provider deltas plus engine-domain tool and
    /// notification events.
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
}
mod outputs {
    //! Engine event output fan-out.

    use crate::records::AgentRunRecordWriter;

    use super::event::StreamEvent;
    use super::printer::EngineEventPrinter;
    use super::sink::EngineEventSink;

    /// Output aggregate for live observations, printing, and durable run records.
    #[derive(Clone, Default)]
    pub struct EngineEventOutputs {
        live_event_sink: Option<EngineEventSink>,
        event_printer: Option<EngineEventPrinter>,
        run_record_writer: Option<AgentRunRecordWriter>,
    }

    impl std::fmt::Debug for EngineEventOutputs {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("EngineEventOutputs")
                .field("has_live_event_sink", &self.live_event_sink.is_some())
                .field("has_event_printer", &self.event_printer.is_some())
                .field("has_run_record_writer", &self.run_record_writer.is_some())
                .finish()
        }
    }

    impl EngineEventOutputs {
        /// Create an empty output aggregate.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Attach live event observation.
        #[must_use]
        pub fn with_live_event_sink(mut self, live_event_sink: Option<EngineEventSink>) -> Self {
            self.live_event_sink = live_event_sink;
            self
        }

        /// Attach human-readable event printing.
        #[must_use]
        pub fn with_event_printer(mut self, event_printer: Option<EngineEventPrinter>) -> Self {
            self.event_printer = event_printer;
            self
        }

        /// Attach durable agent-run record writing.
        #[must_use]
        pub fn with_run_record_writer(
            mut self,
            run_record_writer: Option<AgentRunRecordWriter>,
        ) -> Self {
            self.run_record_writer = run_record_writer;
            self
        }

        pub(crate) fn observe(&self, event: &StreamEvent) {
            if let Some(live_event_sink) = &self.live_event_sink {
                live_event_sink(event);
            }
            if let Some(event_printer) = &self.event_printer {
                event_printer.print(event);
            }
        }

        pub(crate) fn run_record_writer(&self) -> Option<&AgentRunRecordWriter> {
            self.run_record_writer.as_ref()
        }
    }
}
mod printer {
    //! Engine event rendering.

    use std::sync::Arc;

    use super::StreamEvent;

    type EngineEventPrintSink = Arc<dyn Fn(String) + Send + Sync>;

    /// Renders engine events into a caller-provided sink.
    #[derive(Clone)]
    pub struct EngineEventPrinter {
        sink: EngineEventPrintSink,
    }

    impl std::fmt::Debug for EngineEventPrinter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("EngineEventPrinter").finish_non_exhaustive()
        }
    }

    impl EngineEventPrinter {
        /// Create a printer from a text sink.
        #[must_use]
        pub fn new<F>(sink: F) -> Self
        where
            F: Fn(String) + Send + Sync + 'static,
        {
            Self {
                sink: Arc::new(sink),
            }
        }

        /// Render and emit one engine event.
        pub fn print(&self, event: &StreamEvent) {
            (self.sink)(render_engine_event(event));
        }
    }

    fn render_engine_event(event: &StreamEvent) -> String {
        match event {
            StreamEvent::ReasoningDelta { text, .. }
            | StreamEvent::AssistantTextDelta { text, .. }
            | StreamEvent::SystemNotification { text, .. } => text.clone(),
            StreamEvent::AssistantMessageComplete { .. } => "assistant message complete".to_owned(),
            StreamEvent::ToolUseDelta { name, .. } => format!("tool use: {name}"),
            StreamEvent::ToolExecutionStarted { tool_name, .. } => {
                format!("tool started: {tool_name}")
            }
            StreamEvent::ToolExecutionCompleted {
                tool_name,
                is_error,
                is_terminal,
                ..
            } => {
                format!("tool completed: {tool_name} error={is_error} terminal={is_terminal}")
            }
            StreamEvent::ToolExecutionProgress {
                tool_name, output, ..
            } => {
                format!("tool progress: {tool_name}: {output}")
            }
            StreamEvent::ToolExecutionCancelled {
                tool_name, reason, ..
            } => {
                format!("tool cancelled: {tool_name}: {reason}")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::{Arc, Mutex};

        use eos_types::{AgentRunId, JsonObject};

        use super::*;

        #[test]
        fn printer_renders_midflight_tool_events_without_records() {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let captured = lines.clone();
            let printer = EngineEventPrinter::new(move |line| {
                captured.lock().expect("lines lock").push(line);
            });

            printer.print(&StreamEvent::ToolExecutionStarted {
                agent_name: "root".to_owned(),
                agent_run_id: Some(AgentRunId::new_v4()),
                tool_name: "submit_root_outcome".to_owned(),
                tool_input: JsonObject::new(),
                tool_use_id: "toolu_1".parse().expect("valid tool use id"),
            });

            assert_eq!(
                lines.lock().expect("lines lock").as_slice(),
                ["tool started: submit_root_outcome"]
            );
        }
    }
}
mod sink {
    //! Engine event observation sink.

    use std::sync::Arc;

    use eos_types::StartAgentLoopRequest;

    use super::StreamEvent;

    /// Per-run stream/tool/system event sink.
    pub type EngineEventSink = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

    /// Factory for a live event sink bound to one loop start request.
    pub type EngineEventSinkFactory =
        Arc<dyn Fn(&StartAgentLoopRequest) -> Option<EngineEventSink> + Send + Sync>;
}

pub use event::{stamp_identity, AssistantMessageComplete, StreamEvent};
pub use outputs::EngineEventOutputs;
pub use printer::EngineEventPrinter;
pub use sink::{EngineEventSink, EngineEventSinkFactory};
