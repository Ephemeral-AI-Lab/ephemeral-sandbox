//! Tokio-backed agent-loop launcher.

use std::sync::Arc;

use eos_agent_ports::{
    agent_loop_cancel_pair, AgentExecutionMetadataService, AgentLoopLauncher, AgentLoopOutcome,
    AgentLoopOutcomeKind, StartAgentLoopRequest, StartedAgentLoop,
};
use tokio::sync::oneshot;

use super::{AgentLoopExecutor, AgentLoopHooks, AgentLoopToolRegistryFactory, NoopAgentLoopHooks};
use crate::query::{EventSource, EventSourceFactory};

#[derive(Clone)]
pub(crate) enum AgentLoopEventSource {
    Static(Arc<dyn EventSource>),
    Factory(EventSourceFactory),
}

impl AgentLoopEventSource {
    fn resolve(&self, request: &StartAgentLoopRequest) -> Arc<dyn EventSource> {
        match self {
            Self::Static(source) => Arc::clone(source),
            Self::Factory(factory) => factory(request),
        }
    }
}

/// Tokio-backed non-blocking agent-loop launcher.
pub struct TokioAgentLoopLauncher {
    event_source: AgentLoopEventSource,
    loop_hooks: Arc<dyn AgentLoopHooks>,
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    metadata_service: Arc<dyn AgentExecutionMetadataService>,
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
        event_source: Arc<dyn EventSource>,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn AgentExecutionMetadataService>,
    ) -> Self {
        Self::with_hooks(
            AgentLoopEventSource::Static(event_source),
            Arc::new(NoopAgentLoopHooks),
            tool_registry_factory,
            metadata_service,
        )
    }

    /// Build a launcher with a source resolved from each loop request.
    #[must_use]
    pub fn with_event_source_factory(
        event_source_factory: EventSourceFactory,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn AgentExecutionMetadataService>,
    ) -> Self {
        Self::with_hooks(
            AgentLoopEventSource::Factory(event_source_factory),
            Arc::new(NoopAgentLoopHooks),
            tool_registry_factory,
            metadata_service,
        )
    }

    #[must_use]
    pub(crate) fn with_hooks(
        event_source: AgentLoopEventSource,
        loop_hooks: Arc<dyn AgentLoopHooks>,
        tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
        metadata_service: Arc<dyn AgentExecutionMetadataService>,
    ) -> Self {
        Self {
            event_source,
            loop_hooks,
            tool_registry_factory,
            metadata_service,
        }
    }
}

impl AgentLoopLauncher for TokioAgentLoopLauncher {
    fn start_agent_loop(&self, request: StartAgentLoopRequest) -> StartedAgentLoop {
        let (outcome_sender, outcome_receiver) = oneshot::channel();
        let (cancel_handle, cancel_signal) = agent_loop_cancel_pair();
        let event_source = self.event_source.resolve(&request);
        let loop_executor = AgentLoopExecutor::new(
            event_source,
            Arc::clone(&self.loop_hooks),
            Arc::clone(&self.tool_registry_factory),
            Arc::clone(&self.metadata_service),
            cancel_signal,
        );

        tokio::spawn(async move {
            let outcome = loop_executor.execute_agent_loop(request).await;
            let _ignored = outcome_sender.send(outcome);
        });

        StartedAgentLoop {
            outcome_receiver,
            cancel_handle,
        }
    }
}

/// Compatibility facade for callers that have not moved to an injected launcher.
#[must_use]
pub fn start_agent_loop(_request: StartAgentLoopRequest) -> StartedAgentLoop {
    let (outcome_sender, outcome_receiver) = oneshot::channel();
    let (cancel_handle, _cancel_signal) = agent_loop_cancel_pair();
    tokio::spawn(async move {
        let _ignored = outcome_sender.send(AgentLoopOutcome {
            kind: AgentLoopOutcomeKind::LoopFailed {
                error_summary: "start_agent_loop facade is not wired to runtime composition"
                    .to_owned(),
            },
            final_conversation_messages: Vec::new(),
            total_token_count: None,
        });
    });
    StartedAgentLoop {
        outcome_receiver,
        cancel_handle,
    }
}
