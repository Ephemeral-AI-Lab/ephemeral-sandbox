//! Agent-loop lifecycle hooks.

use super::AgentLoopOutcome;
use async_trait::async_trait;

use super::AgentLoopState;

/// Lifecycle hooks for one agent loop.
#[async_trait]
pub(crate) trait AgentLoopHooks: Send + Sync {
    /// Called once after loop state is built.
    async fn on_start(&self, state: &AgentLoopState);
    /// Called at each loop step before one assistant turn executes.
    async fn on_step(&self, state: &AgentLoopState);
    /// Called once before the terminal outcome is returned.
    async fn on_complete(&self, outcome: &AgentLoopOutcome);
}

/// Hook implementation that does nothing.
#[derive(Debug, Default)]
pub(crate) struct NoopAgentLoopHooks;

#[async_trait]
impl AgentLoopHooks for NoopAgentLoopHooks {
    async fn on_start(&self, _state: &AgentLoopState) {}

    async fn on_step(&self, _state: &AgentLoopState) {}

    async fn on_complete(&self, _outcome: &AgentLoopOutcome) {}
}
