//! Tokio-backed agent-loop launcher.

use std::sync::Arc;

use eos_types::{
    AgentLoopCancellation, AgentLoopCancellationHandle, AgentLoopLauncher, AgentRunApi,
    StartAgentLoopRequest, StartedAgentLoop,
};
use tokio::sync::{oneshot, watch};

use super::{
    AgentLoopExecutor, AgentLoopHooks, AgentLoopToolRegistryFactory, BackgroundSessionInputs,
    NoopAgentLoopHooks, ToolCallHookStores, ToolExecutionMetadataReader,
};
use crate::query::{EngineEventSink, ProviderStreamSource, ProviderStreamSourceFactory};

#[derive(Clone, Debug)]
struct WatchAgentLoopCancellation {
    sender: watch::Sender<Option<String>>,
}

impl AgentLoopCancellation for WatchAgentLoopCancellation {
    fn cancel(&self, reason: &str) {
        if self.sender.borrow().is_none() {
            let _ignored = self.sender.send(Some(reason.to_owned()));
        }
    }
}

/// Loop-side cancellation signal.
#[derive(Clone, Debug)]
pub(crate) struct AgentLoopCancelSignal {
    receiver: watch::Receiver<Option<String>>,
}

impl AgentLoopCancelSignal {
    /// Current cancellation reason, if cancellation has been requested.
    #[must_use]
    pub(crate) fn reason(&self) -> Option<String> {
        self.receiver.borrow().clone()
    }
}

/// Build a cancel handle/signal pair for one loop.
#[must_use]
fn agent_loop_cancel_pair() -> (AgentLoopCancellationHandle, AgentLoopCancelSignal) {
    let (sender, receiver) = watch::channel(None);
    (
        Arc::new(WatchAgentLoopCancellation { sender }),
        AgentLoopCancelSignal { receiver },
    )
}

#[derive(Clone)]
pub(crate) enum AgentLoopProviderStream {
    Static(Arc<dyn ProviderStreamSource>),
    Factory(ProviderStreamSourceFactory),
}

/// Tokio-backed non-blocking agent-loop launcher.
pub struct TokioAgentLoopLauncher {
    provider_stream_source: AgentLoopProviderStream,
    loop_hooks: Arc<dyn AgentLoopHooks>,
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    metadata_service: Arc<dyn ToolExecutionMetadataReader>,
    background_dependencies: Option<BackgroundSessionInputs>,
    hook_dependencies: Option<ToolCallHookStores>,
    event_sink: Option<EngineEventSink>,
}

impl std::fmt::Debug for TokioAgentLoopLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioAgentLoopLauncher")
            .finish_non_exhaustive()
    }
}

impl TokioAgentLoopLauncher {
    /// Build a Tokio-backed launcher from engine-owned loop services.
    #[must_use]
    pub fn new(
        provider_stream_source: Arc<dyn ProviderStreamSource>,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self::with_hooks(
            AgentLoopProviderStream::Static(provider_stream_source),
            Arc::new(NoopAgentLoopHooks),
            tool_registry_factory,
            metadata_service,
        )
    }

    /// Build a launcher with a source resolved from each loop request.
    #[must_use]
    pub fn with_provider_stream_source_factory(
        provider_stream_source_factory: ProviderStreamSourceFactory,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self::with_hooks(
            AgentLoopProviderStream::Factory(provider_stream_source_factory),
            Arc::new(NoopAgentLoopHooks),
            tool_registry_factory,
            metadata_service,
        )
    }

    #[must_use]
    pub(crate) fn with_hooks(
        provider_stream_source: AgentLoopProviderStream,
        loop_hooks: Arc<dyn AgentLoopHooks>,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self {
            provider_stream_source,
            loop_hooks,
            tool_registry_factory,
            metadata_service,
            background_dependencies: None,
            hook_dependencies: None,
            event_sink: None,
        }
    }

    /// Attach runtime ports for engine-owned background managers.
    #[must_use]
    pub fn with_background_dependencies(mut self, dependencies: BackgroundSessionInputs) -> Self {
        self.background_dependencies = Some(dependencies);
        self
    }

    /// Attach runtime stores for engine-owned tool-call hooks.
    #[must_use]
    pub fn with_hook_dependencies(mut self, dependencies: ToolCallHookStores) -> Self {
        self.hook_dependencies = Some(dependencies);
        self
    }

    /// Attach an optional stream-event sink invoked by each run.
    #[must_use]
    pub fn with_event_sink(mut self, sink: Option<EngineEventSink>) -> Self {
        self.event_sink = sink;
        self
    }
}

impl AgentLoopLauncher for TokioAgentLoopLauncher {
    fn start_agent_loop(
        &self,
        request: StartAgentLoopRequest,
        agent_run_api: Arc<dyn AgentRunApi>,
    ) -> StartedAgentLoop {
        let (outcome_sender, outcome_receiver) = oneshot::channel();
        let (cancel_handle, cancel_signal) = agent_loop_cancel_pair();
        let loop_executor = AgentLoopExecutor::new(
            self.provider_stream_source.clone(),
            Arc::clone(&self.loop_hooks),
            Arc::clone(&self.tool_registry_factory),
            Arc::clone(&self.metadata_service),
            cancel_signal,
            self.background_dependencies.clone(),
            self.hook_dependencies.clone(),
            self.event_sink.clone(),
            agent_run_api,
        );

        tokio::spawn(async move {
            let outcome = loop_executor.execute_agent_loop(request).await;
            let _ignored = outcome_sender.send(outcome);
        });

        StartedAgentLoop {
            outcome: Box::pin(async move { outcome_receiver.await.ok() }),
            cancellation: cancel_handle,
        }
    }
}
