//! Provider request/message helpers and provider-stream seams.

pub(crate) mod messages {
    //! Provider-facing message preparation.

    use std::collections::BTreeSet;

    use eos_llm_client::{ContentBlock, Message, MessageRole};
    use eos_types::ToolUseId;

    pub(crate) fn build_provider_messages(messages: &[Message]) -> Vec<Message> {
        let mut sanitized = messages.to_vec();
        drop_unmatched_tool_blocks(&mut sanitized);
        sanitized
            .into_iter()
            .filter(|message| !message.content.is_empty())
            .collect()
    }

    fn message_tool_use_ids(message: &Message) -> BTreeSet<ToolUseId> {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    }

    fn message_tool_result_ids(message: &Message) -> BTreeSet<ToolUseId> {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    }

    fn remove_tool_uses(message: &mut Message, tool_use_ids: &BTreeSet<ToolUseId>) {
        if tool_use_ids.is_empty() {
            return;
        }
        message.content.retain(|block| match block {
            ContentBlock::ToolUse { tool_use_id, .. } => !tool_use_ids.contains(tool_use_id),
            _ => true,
        });
    }

    fn remove_tool_results(message: &mut Message, tool_result_ids: &BTreeSet<ToolUseId>) {
        if tool_result_ids.is_empty() {
            return;
        }
        message.content.retain(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => !tool_result_ids.contains(tool_use_id),
            _ => true,
        });
    }

    fn drop_unmatched_tool_blocks(messages: &mut [Message]) {
        let mut pending_tool_use_ids = BTreeSet::new();
        let mut pending_message_index = None;

        for message_index in 0..messages.len() {
            let tool_use_ids = message_tool_use_ids(&messages[message_index]);
            let mut tool_result_ids = message_tool_result_ids(&messages[message_index]);
            let mut matched_pending_tool_uses = false;

            if !pending_tool_use_ids.is_empty() {
                let current_message_matches = messages[message_index].role == MessageRole::User
                    && pending_tool_use_ids.is_subset(&tool_result_ids);

                if current_message_matches {
                    let unmatched_result_ids = tool_result_ids
                        .difference(&pending_tool_use_ids)
                        .cloned()
                        .collect();
                    remove_tool_results(&mut messages[message_index], &unmatched_result_ids);
                    pending_tool_use_ids.clear();
                    pending_message_index = None;
                    matched_pending_tool_uses = true;
                } else {
                    if let Some(index) = pending_message_index {
                        remove_tool_uses(&mut messages[index], &pending_tool_use_ids);
                    }
                    pending_tool_use_ids.clear();
                    pending_message_index = None;
                    tool_result_ids = message_tool_result_ids(&messages[message_index]);
                }
            }

            if !tool_result_ids.is_empty() && tool_use_ids.is_empty() && !matched_pending_tool_uses
            {
                messages[message_index]
                    .content
                    .retain(|block| !matches!(block, ContentBlock::ToolResult { .. }));
            }

            let tool_use_ids = message_tool_use_ids(&messages[message_index]);
            if !tool_use_ids.is_empty() {
                pending_tool_use_ids = tool_use_ids;
                pending_message_index = Some(message_index);
            }
        }

        if !pending_tool_use_ids.is_empty() {
            if let Some(index) = pending_message_index {
                remove_tool_uses(&mut messages[index], &pending_tool_use_ids);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::expect_used)]

        use eos_types::JsonObject;

        use super::*;

        fn id(value: &str) -> ToolUseId {
            value.parse().expect("tool use id")
        }

        fn assistant_tool_use(value: &str) -> Message {
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    tool_use_id: id(value),
                    name: "read_file".to_owned(),
                    input: JsonObject::new(),
                }],
            }
        }

        fn assistant_tool_uses(values: &[&str]) -> Message {
            Message {
                role: MessageRole::Assistant,
                content: values
                    .iter()
                    .map(|value| ContentBlock::ToolUse {
                        tool_use_id: id(value),
                        name: "read_file".to_owned(),
                        input: JsonObject::new(),
                    })
                    .collect(),
            }
        }

        fn user_tool_results(values: &[&str]) -> Message {
            Message {
                role: MessageRole::User,
                content: values
                    .iter()
                    .map(|value| ContentBlock::ToolResult {
                        tool_use_id: id(value),
                        content: format!("result {value}"),
                        is_error: false,
                        metadata: JsonObject::new(),
                        is_terminal: false,
                    })
                    .collect(),
            }
        }

        #[test]
        fn keeps_matched_tool_pair() {
            let messages = vec![
                assistant_tool_use("toolu_a"),
                user_tool_results(&["toolu_a"]),
            ];

            let sanitized = build_provider_messages(&messages);

            assert_eq!(sanitized, messages);
        }

        #[test]
        fn drops_trailing_tool_use_from_provider_copy_only() {
            let messages = vec![assistant_tool_use("toolu_a")];

            let sanitized = build_provider_messages(&messages);

            assert!(sanitized.is_empty());
            assert_eq!(messages[0].content.len(), 1);
        }

        #[test]
        fn drops_orphan_tool_result() {
            let messages = vec![user_tool_results(&["toolu_a"])];

            let sanitized = build_provider_messages(&messages);

            assert!(sanitized.is_empty());
        }

        #[test]
        fn drops_extra_tool_result_from_matched_result_message() {
            let messages = vec![
                assistant_tool_use("toolu_a"),
                user_tool_results(&["toolu_a", "toolu_extra"]),
            ];

            let sanitized = build_provider_messages(&messages);

            assert_eq!(sanitized.len(), 2);
            assert_eq!(
                message_tool_result_ids(&sanitized[1]),
                BTreeSet::from([id("toolu_a")])
            );
        }

        #[test]
        fn drops_partial_mismatch_on_next_message() {
            let messages = vec![
                assistant_tool_use("toolu_a"),
                user_tool_results(&["toolu_b"]),
            ];

            let sanitized = build_provider_messages(&messages);

            assert!(sanitized.is_empty());
        }

        #[test]
        fn keeps_multi_tool_pair_when_all_results_are_present() {
            let messages = vec![
                assistant_tool_uses(&["toolu_a", "toolu_b"]),
                user_tool_results(&["toolu_b", "toolu_a"]),
            ];

            let sanitized = build_provider_messages(&messages);

            assert_eq!(sanitized, messages);
        }

        #[test]
        fn drops_partial_multi_tool_pair() {
            let messages = vec![
                assistant_tool_uses(&["toolu_a", "toolu_b"]),
                user_tool_results(&["toolu_a"]),
            ];

            let sanitized = build_provider_messages(&messages);

            assert!(sanitized.is_empty());
        }

        #[test]
        fn keeps_assistant_text_when_dropping_unmatched_tool_use() {
            let messages = vec![Message {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "partial answer".to_owned(),
                    },
                    ContentBlock::ToolUse {
                        tool_use_id: id("toolu_a"),
                        name: "read_file".to_owned(),
                        input: JsonObject::new(),
                    },
                ],
            }];

            let sanitized = build_provider_messages(&messages);

            assert_eq!(sanitized.len(), 1);
            assert!(matches!(
                sanitized[0].content.as_slice(),
                [ContentBlock::Text { text }] if text == "partial answer"
            ));
        }
    }
}
mod source {
    //! Provider stream source contracts and production LLM adapter.

    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_llm_client::{LlmClient, LlmRequest, LlmStreamEvent};
    use eos_types::{AgentRunRuntimeSnapshot, StartAgentLoopRequest};
    use futures::{Stream, StreamExt};

    use crate::event::{AssistantMessageComplete, StreamEvent};
    use crate::EngineError;

    /// The engine stream returned by one model turn.
    pub type EngineStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>;

    /// Per-loop provider stream source factory.
    pub type ProviderStreamSourceFactory = Arc<
        dyn Fn(&StartAgentLoopRequest, &AgentRunRuntimeSnapshot) -> Arc<dyn ProviderStreamSource>
            + Send
            + Sync,
    >;

    /// A per-agent stream source. Production adapts an `LlmClient`; tests can replay
    /// scripted engine events while still exercising the real loop.
    #[async_trait]
    pub trait ProviderStreamSource: Send + Sync {
        /// Open one model turn for `request`.
        ///
        /// # Errors
        /// Returns [`EngineError`] for request construction or stream setup faults.
        async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError>;
    }

    /// LLM-backed provider stream source.
    #[derive(Clone)]
    pub struct LlmProviderStreamSource {
        client: Arc<dyn LlmClient>,
    }

    impl std::fmt::Debug for LlmProviderStreamSource {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LlmProviderStreamSource")
                .finish_non_exhaustive()
        }
    }

    impl LlmProviderStreamSource {
        /// Create a provider stream source from a provider-neutral client.
        #[must_use]
        pub fn new(client: Arc<dyn LlmClient>) -> Self {
            Self { client }
        }
    }

    fn adapt_event(event: LlmStreamEvent) -> Result<StreamEvent, EngineError> {
        match event {
            LlmStreamEvent::AssistantTextDelta { text } => Ok(StreamEvent::AssistantTextDelta {
                agent_name: String::new(),
                agent_run_id: None,
                text,
            }),
            LlmStreamEvent::ReasoningDelta { text } => Ok(StreamEvent::ReasoningDelta {
                agent_name: String::new(),
                agent_run_id: None,
                text,
            }),
            LlmStreamEvent::ToolUseDelta {
                tool_use_id,
                name,
                input,
            } => Ok(StreamEvent::ToolUseDelta {
                agent_name: String::new(),
                agent_run_id: None,
                tool_use_id,
                name,
                input,
            }),
            LlmStreamEvent::AssistantMessageComplete {
                message,
                usage,
                stop_reason,
            } => Ok(StreamEvent::AssistantMessageComplete {
                agent_name: String::new(),
                agent_run_id: None,
                payload: Box::new(AssistantMessageComplete {
                    message,
                    usage,
                    stop_reason,
                }),
            }),
            _ => Err(EngineError::Internal(
                "unsupported provider stream event variant".to_owned(),
            )),
        }
    }

    #[async_trait]
    impl ProviderStreamSource for LlmProviderStreamSource {
        async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
            let stream = self.client.stream_message(request.clone()).await?;
            Ok(Box::pin(stream.map(|item| match item {
                Ok(event) => adapt_event(event),
                Err(error) => Err(EngineError::from(error)),
            })))
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::expect_used)]

        use async_trait::async_trait;
        use eos_llm_client::{
            ContentBlock, LlmClient, LlmRequest, LlmStream, LlmStreamEvent, Message, MessageRole,
            ProviderError, StopReason, UsageSnapshot,
        };
        use futures::StreamExt;
        use serde_json::json;

        use super::*;

        #[derive(Debug, Clone)]
        struct ScriptedClient {
            stream: Vec<Result<LlmStreamEvent, ProviderError>>,
        }

        #[async_trait]
        impl LlmClient for ScriptedClient {
            async fn stream_message(
                &self,
                _request: LlmRequest,
            ) -> Result<LlmStream, ProviderError> {
                Ok(Box::pin(futures::stream::iter(self.stream.clone())))
            }
        }

        fn request() -> LlmRequest {
            LlmRequest::builder("test-model").build()
        }

        #[tokio::test]
        async fn provider_stream_source_maps_model_stream_events() {
            let final_message = Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "done".to_owned(),
                }],
            };
            let source = LlmProviderStreamSource::new(Arc::new(ScriptedClient {
                stream: vec![
                    Ok(LlmStreamEvent::AssistantTextDelta {
                        text: "hello".to_owned(),
                    }),
                    Ok(LlmStreamEvent::ReasoningDelta {
                        text: "thinking".to_owned(),
                    }),
                    Ok(LlmStreamEvent::ToolUseDelta {
                        tool_use_id: "toolu-1".parse().expect("tool id"),
                        name: "read_file".to_owned(),
                        input: json!({"path": "README.md"})
                            .as_object()
                            .expect("object")
                            .clone(),
                    }),
                    Ok(LlmStreamEvent::AssistantMessageComplete {
                        message: final_message.clone(),
                        usage: UsageSnapshot {
                            input_tokens: 7,
                            output_tokens: 3,
                        },
                        stop_reason: Some(StopReason::EndTurn),
                    }),
                ],
            }));

            let mut stream = source.stream(&request()).await.expect("stream");
            assert!(matches!(
                stream.next().await.expect("text").expect("ok"),
                StreamEvent::AssistantTextDelta { text, .. } if text == "hello"
            ));
            assert!(matches!(
                stream.next().await.expect("reasoning").expect("ok"),
                StreamEvent::ReasoningDelta { text, .. } if text == "thinking"
            ));
            assert!(matches!(
                stream.next().await.expect("tool").expect("ok"),
                StreamEvent::ToolUseDelta { name, input, .. }
                    if name == "read_file" && input["path"] == json!("README.md")
            ));
            assert!(matches!(
                stream.next().await.expect("complete").expect("ok"),
                StreamEvent::AssistantMessageComplete { payload, .. }
                    if payload.message == final_message
                        && payload.usage.input_tokens == 7
                        && payload.stop_reason == Some(StopReason::EndTurn)
            ));
            assert!(stream.next().await.is_none());
        }

        #[tokio::test]
        async fn provider_stream_source_propagates_provider_stream_errors() {
            let source = LlmProviderStreamSource::new(Arc::new(ScriptedClient {
                stream: vec![Err(ProviderError::transport("connection reset"))],
            }));

            let mut stream = source.stream(&request()).await.expect("stream");
            let err = stream
                .next()
                .await
                .expect("error item")
                .expect_err("provider error");

            assert!(matches!(err, EngineError::Provider(_)));
            assert!(err.to_string().contains("connection reset"));
        }
    }
}

pub use source::{
    EngineStream, LlmProviderStreamSource, ProviderStreamSource, ProviderStreamSourceFactory,
};
