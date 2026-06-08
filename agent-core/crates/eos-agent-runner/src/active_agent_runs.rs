//! In-process active agent-run waiter registry.

use std::collections::HashMap;
use std::sync::Arc;

use eos_agent_ports::{AgentLoopCancelHandle, AgentRunError, AgentRunOutcome};
use eos_types::AgentRunId;
use tokio::sync::{watch, Mutex};

/// Registry of active in-process agent runs.
#[derive(Clone, Default)]
pub struct ActiveAgentRuns {
    inner: Arc<Mutex<HashMap<AgentRunId, ActiveAgentRun>>>,
}

impl std::fmt::Debug for ActiveAgentRuns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveAgentRuns").finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct ActiveAgentRun {
    outcome_tx: watch::Sender<Option<AgentRunOutcome>>,
    cancel_handle: AgentLoopCancelHandle,
}

impl ActiveAgentRuns {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn insert(
        &self,
        agent_run_id: AgentRunId,
        cancel_handle: AgentLoopCancelHandle,
    ) {
        let (outcome_tx, _) = watch::channel(None);
        self.inner.lock().await.insert(
            agent_run_id,
            ActiveAgentRun {
                outcome_tx,
                cancel_handle,
            },
        );
    }

    pub(crate) async fn take(&self, agent_run_id: &AgentRunId) -> Option<ActiveAgentRunCompletion> {
        self.inner
            .lock()
            .await
            .remove(agent_run_id)
            .map(|handle| ActiveAgentRunCompletion {
                outcome_tx: handle.outcome_tx,
                cancel_handle: handle.cancel_handle,
            })
    }

    pub(crate) async fn current_outcome(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Option<AgentRunOutcome> {
        self.inner
            .lock()
            .await
            .get(agent_run_id)
            .and_then(|handle| handle.outcome_tx.borrow().clone())
    }

    pub(crate) async fn subscribe(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<watch::Receiver<Option<AgentRunOutcome>>, AgentRunError> {
        self.inner
            .lock()
            .await
            .get(agent_run_id)
            .map(|handle| handle.outcome_tx.subscribe())
            .ok_or_else(|| AgentRunError::NotActiveInProcess(agent_run_id.clone()))
    }
}

pub(crate) struct ActiveAgentRunCompletion {
    outcome_tx: watch::Sender<Option<AgentRunOutcome>>,
    cancel_handle: AgentLoopCancelHandle,
}

impl ActiveAgentRunCompletion {
    pub(crate) fn cancel(&self, reason: &str) {
        self.cancel_handle.cancel(reason.to_owned());
    }

    pub(crate) fn publish(self, outcome: AgentRunOutcome) {
        let _ignored = self.outcome_tx.send(Some(outcome));
    }
}
