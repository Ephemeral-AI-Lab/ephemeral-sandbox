//! Engine/provider service group.

use std::sync::Arc;

use eos_llm_client::LlmClient;

use super::EventSourceFactory;

/// Provider stream dependencies used by the engine loop.
#[derive(Clone)]
pub(crate) struct EngineService {
    pub(crate) llm_client: Arc<dyn LlmClient>,
    pub(crate) event_source_factory: Option<EventSourceFactory>,
}

impl std::fmt::Debug for EngineService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineService")
            .field(
                "has_event_source_factory",
                &self.event_source_factory.is_some(),
            )
            .finish_non_exhaustive()
    }
}
