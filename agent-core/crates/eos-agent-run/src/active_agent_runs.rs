//! In-process active agent-run waiter registry.

use std::collections::HashMap;
use std::sync::Arc;

use eos_types::{AgentLoopCancellationHandle, AgentRunError, AgentRunId, AgentRunOutcome};
use tokio::sync::{watch, Mutex};

/// Registry of active in-process agent runs.
#[derive(Clone, Default)]
pub struct ActiveAgentRunRegistry {
    active_runs: Arc<Mutex<HashMap<AgentRunId, ActiveAgentRunHandle>>>,
}

impl std::fmt::Debug for ActiveAgentRunRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveAgentRunRegistry")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct ActiveAgentRunHandle {
    agent_run_id: AgentRunId,
    completion_tx: watch::Sender<Option<AgentRunOutcome>>,
    loop_cancellation: AgentLoopCancellationHandle,
}

impl ActiveAgentRunRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn insert(
        &self,
        agent_run_id: AgentRunId,
        loop_cancellation: AgentLoopCancellationHandle,
    ) {
        let (completion_tx, _) = watch::channel(None);
        self.active_runs.lock().await.insert(
            agent_run_id.clone(),
            ActiveAgentRunHandle {
                agent_run_id,
                completion_tx,
                loop_cancellation,
            },
        );
    }

    pub(crate) async fn take(&self, agent_run_id: &AgentRunId) -> Option<ActiveAgentRunCompletion> {
        self.active_runs
            .lock()
            .await
            .remove(agent_run_id)
            .map(|handle| ActiveAgentRunCompletion {
                agent_run_id: handle.agent_run_id,
                completion_tx: handle.completion_tx,
                loop_cancellation: handle.loop_cancellation,
            })
    }

    pub(crate) async fn current_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Option<AgentRunOutcome> {
        self.active_runs
            .lock()
            .await
            .get(agent_run_id)
            .and_then(|handle| handle.completion_tx.borrow().clone())
    }

    pub(crate) async fn subscribe(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<watch::Receiver<Option<AgentRunOutcome>>, AgentRunError> {
        self.active_runs
            .lock()
            .await
            .get(agent_run_id)
            .map(|handle| handle.completion_tx.subscribe())
            .ok_or_else(|| AgentRunError::NotActiveInProcess(agent_run_id.clone()))
    }
}

pub(crate) struct ActiveAgentRunCompletion {
    agent_run_id: AgentRunId,
    completion_tx: watch::Sender<Option<AgentRunOutcome>>,
    loop_cancellation: AgentLoopCancellationHandle,
}

impl ActiveAgentRunCompletion {
    pub(crate) fn agent_run_id(&self) -> &AgentRunId {
        &self.agent_run_id
    }

    pub(crate) fn cancel(&self, reason: &str) {
        self.loop_cancellation.cancel(reason);
    }

    pub(crate) fn publish(self, outcome: AgentRunOutcome) {
        let _ignored = self.completion_tx.send(Some(outcome));
    }
}
