//! Production event source: adapt `LlmClient` events into engine events.

use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::{LlmClient, LlmRequest, LlmStreamEvent};
use futures::StreamExt;

use crate::events::{AssistantMessageComplete, StreamEvent};
use crate::query::EngineStream;
use crate::EngineError;

/// Provider-backed event source.
#[derive(Clone)]
pub struct ProviderEventSource {
    client: Arc<dyn LlmClient>,
}

impl std::fmt::Debug for ProviderEventSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEventSource")
            .finish_non_exhaustive()
    }
}

impl ProviderEventSource {
    /// Create an event source from a provider-neutral client.
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
impl crate::query::EventSource for ProviderEventSource {
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError> {
        let stream = self.client.stream_message(request.clone()).await?;
        Ok(Box::pin(stream.map(|item| match item {
            Ok(event) => adapt_event(event),
            Err(error) => Err(EngineError::from(error)),
        })))
    }
}
