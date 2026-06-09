//! In-process active agent-run waiter registry.

use std::collections::HashMap;
use std::sync::Arc;

use eos_types::{AgentLoopCancellationHandle, AgentRunError, AgentRunId, AgentRunOutcome};
use tokio::sync::{watch, Mutex};

use crate::records::AgentRunRecordHandle;

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
    cancel_handle: AgentLoopCancellationHandle,
    message_record: Option<ActiveAgentRunRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveAgentRunRecord {
    pub(crate) handle: AgentRunRecordHandle,
    pub(crate) initial_message_count: usize,
}

impl ActiveAgentRunRecord {
    pub(crate) fn new(handle: AgentRunRecordHandle, initial_message_count: usize) -> Self {
        Self {
            handle,
            initial_message_count,
        }
    }
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
        cancel_handle: AgentLoopCancellationHandle,
        message_record: Option<ActiveAgentRunRecord>,
    ) {
        let (outcome_tx, _) = watch::channel(None);
        self.inner.lock().await.insert(
            agent_run_id,
            ActiveAgentRun {
                outcome_tx,
                cancel_handle,
                message_record,
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
                message_record: handle.message_record,
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
    cancel_handle: AgentLoopCancellationHandle,
    message_record: Option<ActiveAgentRunRecord>,
}

impl ActiveAgentRunCompletion {
    pub(crate) fn cancel(&self, reason: &str) {
        self.cancel_handle.cancel(reason);
    }

    pub(crate) fn take_message_record(&mut self) -> Option<ActiveAgentRunRecord> {
        self.message_record.take()
    }

    pub(crate) fn publish(self, outcome: AgentRunOutcome) {
        let _ignored = self.outcome_tx.send(Some(outcome));
    }
}
