//! Engine/provider service group.

use std::sync::Arc;
use std::time::Duration;

use eos_llm_client::LlmClient;

use crate::RuntimeConfig;

use super::ProviderStreamSourceFactory;

/// Provider stream dependencies used by the engine loop.
#[derive(Clone)]
pub(crate) struct EngineService {
    pub(crate) llm_client: Arc<dyn LlmClient>,
    pub(crate) provider_stream_source_factory: Option<ProviderStreamSourceFactory>,
    pub(crate) runtime_config: RuntimeConfig,
}

impl EngineService {
    pub(crate) fn command_session_completion_poll_interval(&self) -> Duration {
        Duration::from_millis(
            self.runtime_config
                .command_session_completion_poll_interval_ms,
        )
    }
}

impl std::fmt::Debug for EngineService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineService")
            .field(
                "has_provider_stream_source_factory",
                &self.provider_stream_source_factory.is_some(),
            )
            .field(
                "command_session_completion_poll_interval_ms",
                &self
                    .runtime_config
                    .command_session_completion_poll_interval_ms,
            )
            .finish_non_exhaustive()
    }
}
