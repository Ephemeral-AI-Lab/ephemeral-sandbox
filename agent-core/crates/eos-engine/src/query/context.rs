//! Query provider/event-source seams.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::LlmRequest;
use eos_types::AgentState;
use futures::Stream;

use crate::{EngineError, StartAgentLoopRequest, StreamEvent};

/// The engine stream returned by one model turn.
pub type EngineStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>;

/// Per-loop event source factory.
pub type EventSourceFactory =
    Arc<dyn Fn(&StartAgentLoopRequest, &AgentState) -> Arc<dyn EventSource> + Send + Sync>;

/// Per-run stream-event callback.
pub type EventCallback = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// A per-agent stream source. Production adapts an `LlmClient`; tests can replay
/// scripted engine events while still exercising the real loop.
#[async_trait]
pub trait EventSource: Send + Sync {
    /// Open one model turn for `request`.
    ///
    /// # Errors
    /// Returns [`EngineError`] for request construction or stream setup faults.
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError>;
}
