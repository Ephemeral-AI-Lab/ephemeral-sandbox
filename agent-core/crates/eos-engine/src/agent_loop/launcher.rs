//! Tokio-backed agent-loop launcher.

use std::sync::Arc;

use eos_types::{
    AgentLoopCancellation, AgentLoopCancellationHandle, AgentLoopCompletion, AgentLoopLauncher,
    AgentLoopOutcome, AgentLoopOutcomeKind, AgentRunApi, StartAgentLoopRequest, StartedAgentLoop,
};
use tokio::sync::{oneshot, watch};

use super::{
    AgentLoopExecutor, AgentLoopExecutorInput, AgentLoopToolRegistryFactory,
    BackgroundSessionRuntimeFactory, ToolCallHookStores, ToolExecutionMetadataReader,
};
use crate::event::EngineEventOutputs;
use crate::provider_stream::{ProviderStreamSource, ProviderStreamSourceFactory};

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

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (_handle, signal) = agent_loop_cancel_pair();
        signal
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
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    execution_metadata_reader: Arc<dyn ToolExecutionMetadataReader>,
    background_sessions: Option<BackgroundSessionRuntimeFactory>,
    hook_stores: Option<ToolCallHookStores>,
    event_outputs: EngineEventOutputs,
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
        execution_metadata_reader: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self::new_with_provider_stream(
            AgentLoopProviderStream::Static(provider_stream_source),
            tool_registry_factory,
            execution_metadata_reader,
        )
    }

    /// Build a launcher with a source resolved from each loop request.
    #[must_use]
    pub fn with_provider_stream_source_factory(
        provider_stream_source_factory: ProviderStreamSourceFactory,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        execution_metadata_reader: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self::new_with_provider_stream(
            AgentLoopProviderStream::Factory(provider_stream_source_factory),
            tool_registry_factory,
            execution_metadata_reader,
        )
    }

    #[must_use]
    fn new_with_provider_stream(
        provider_stream_source: AgentLoopProviderStream,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        execution_metadata_reader: Arc<dyn ToolExecutionMetadataReader>,
    ) -> Self {
        Self {
            provider_stream_source,
            tool_registry_factory,
            execution_metadata_reader,
            background_sessions: None,
            hook_stores: None,
            event_outputs: EngineEventOutputs::new(),
        }
    }

    /// Attach runtime contracts for engine-owned background managers.
    #[must_use]
    pub fn with_background_sessions(mut self, inputs: BackgroundSessionRuntimeFactory) -> Self {
        self.background_sessions = Some(inputs);
        self
    }

    /// Attach runtime stores for engine-owned tool-call hooks.
    #[must_use]
    pub fn with_tool_call_hook_stores(mut self, stores: ToolCallHookStores) -> Self {
        self.hook_stores = Some(stores);
        self
    }

    /// Attach event output fan-out for each run.
    #[must_use]
    pub fn with_event_outputs(mut self, event_outputs: EngineEventOutputs) -> Self {
        self.event_outputs = event_outputs;
        self
    }
}

impl AgentLoopLauncher for TokioAgentLoopLauncher {
    fn start_agent_loop(
        &self,
        request: StartAgentLoopRequest,
        agent_run_api: Arc<dyn AgentRunApi>,
    ) -> StartedAgentLoop {
        let (completion_sender, completion_wait) = oneshot::channel();
        let (cancel_handle, cancel_signal) = agent_loop_cancel_pair();
        let loop_executor = AgentLoopExecutor::new(AgentLoopExecutorInput {
            provider_stream_source: self.provider_stream_source.clone(),
            tool_registry_factory: Arc::clone(&self.tool_registry_factory),
            execution_metadata_reader: Arc::clone(&self.execution_metadata_reader),
            cancel_signal,
            background_sessions: self.background_sessions.clone(),
            hook_stores: self.hook_stores.clone(),
            event_outputs: self.event_outputs.clone(),
            agent_run_api,
        });

        tokio::spawn(async move {
            let outcome = loop_executor.execute_agent_loop(request).await;
            let _ignored = completion_sender.send(outcome);
        });

        StartedAgentLoop {
            completion: AgentLoopCompletion::new(async move {
                completion_wait.await.unwrap_or_else(|_| AgentLoopOutcome {
                    kind: AgentLoopOutcomeKind::LoopFailed {
                        error_summary: "agent loop outcome sender dropped".to_owned(),
                    },
                    final_conversation_messages: Vec::new(),
                    total_token_count: None,
                })
            }),
            cancellation: cancel_handle,
        }
    }
}
