//! Production provider stream source: adapt `LlmClient` events into engine events.

use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::{LlmClient, LlmRequest, LlmStreamEvent};
use futures::StreamExt;

use crate::query::EngineStream;
use crate::telemetry::{AssistantMessageComplete, StreamEvent};
use crate::EngineError;

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
impl crate::query::ProviderStreamSource for LlmProviderStreamSource {
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

    use crate::query::ProviderStreamSource;

    use super::*;

    #[derive(Debug, Clone)]
    struct ScriptedClient {
        stream: Vec<Result<LlmStreamEvent, ProviderError>>,
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
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
