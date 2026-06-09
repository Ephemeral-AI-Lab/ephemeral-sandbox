//! Tokio-backed agent-loop launcher.

use std::sync::Arc;

use tokio::sync::{oneshot, watch};

use super::{
    AgentExecutionMetadataService, AgentLoopBackgroundDependencies, AgentLoopExecutor,
    AgentLoopHookDependencies, AgentLoopHooks, AgentLoopOutcome, AgentLoopOutcomeKind,
    AgentLoopToolRegistryFactory, NoopAgentLoopHooks, StartAgentLoopRequest,
};
use crate::query::{EventCallback, EventSource, EventSourceFactory};

/// Public non-blocking launcher for agent loops.
pub trait AgentLoopLauncher: Send + Sync {
    /// Start an agent loop and return immediately with its outcome receiver.
    fn start_agent_loop(&self, request: StartAgentLoopRequest) -> StartedAgentLoop;
}

/// Handle returned after an agent loop has been started.
#[derive(Debug)]
pub struct StartedAgentLoop {
    /// Receives the terminal loop outcome.
    pub outcome_receiver: oneshot::Receiver<AgentLoopOutcome>,
    /// Cooperative cancellation handle for the running loop.
    pub cancel_handle: AgentLoopCancelHandle,
}

/// Cooperative cancellation handle for one agent loop.
#[derive(Clone, Debug)]
pub struct AgentLoopCancelHandle {
    sender: watch::Sender<Option<String>>,
}

impl AgentLoopCancelHandle {
    /// Request loop cancellation. The first reason wins.
    pub fn cancel(&self, reason: impl Into<String>) {
        if self.sender.borrow().is_none() {
            let _ignored = self.sender.send(Some(reason.into()));
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
fn agent_loop_cancel_pair() -> (AgentLoopCancelHandle, AgentLoopCancelSignal) {
    let (sender, receiver) = watch::channel(None);
    (
        AgentLoopCancelHandle { sender },
        AgentLoopCancelSignal { receiver },
    )
}

#[derive(Clone)]
pub(crate) enum AgentLoopEventSource {
    Static(Arc<dyn EventSource>),
    Factory(EventSourceFactory),
}

/// Tokio-backed non-blocking agent-loop launcher.
pub struct TokioAgentLoopLauncher {
    event_source: AgentLoopEventSource,
    loop_hooks: Arc<dyn AgentLoopHooks>,
    tool_registry_factory: Arc<dyn AgentLoopToolRegistryFactory>,
    metadata_service: Arc<dyn AgentExecutionMetadataService>,
    background_dependencies: Option<AgentLoopBackgroundDependencies>,
    hook_dependencies: Option<AgentLoopHookDependencies>,
    event_callback: Option<EventCallback>,
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
            background_dependencies: None,
            hook_dependencies: None,
            event_callback: None,
        }
    }

    /// Attach runtime ports for engine-owned background managers.
    #[must_use]
    pub fn with_background_dependencies(
        mut self,
        dependencies: AgentLoopBackgroundDependencies,
    ) -> Self {
        self.background_dependencies = Some(dependencies);
        self
    }

    /// Attach runtime stores for engine-owned tool-call hooks.
    #[must_use]
    pub fn with_hook_dependencies(mut self, dependencies: AgentLoopHookDependencies) -> Self {
        self.hook_dependencies = Some(dependencies);
        self
    }

    /// Attach an optional stream-event callback invoked by each run.
    #[must_use]
    pub fn with_event_callback(mut self, callback: Option<EventCallback>) -> Self {
        self.event_callback = callback;
        self
    }
}

impl AgentLoopLauncher for TokioAgentLoopLauncher {
    fn start_agent_loop(&self, request: StartAgentLoopRequest) -> StartedAgentLoop {
        let (outcome_sender, outcome_receiver) = oneshot::channel();
        let (cancel_handle, cancel_signal) = agent_loop_cancel_pair();
        let loop_executor = AgentLoopExecutor::new(
            self.event_source.clone(),
            Arc::clone(&self.loop_hooks),
            Arc::clone(&self.tool_registry_factory),
            Arc::clone(&self.metadata_service),
            cancel_signal,
            self.background_dependencies.clone(),
            self.hook_dependencies.clone(),
            self.event_callback.clone(),
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
