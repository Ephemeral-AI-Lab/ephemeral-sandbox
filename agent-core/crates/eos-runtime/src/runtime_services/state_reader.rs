//! Narrow read-side handle over agent-core persisted state.
//!
//! [`RuntimeServices::state_reader`](super::RuntimeServices::state_reader) hands
//! the backend composition root the crate-owned store traits it needs to join
//! its own lifecycle rows with agent-core request/task/agent-run state — never a
//! `sqlx` pool or the table layout (spec §State Reader). It is intentionally
//! narrow: only the three read stores the backend API consumes, exposed as
//! `Arc<dyn …Store>` so the backend couples to the typed contract, not the DB.

use std::sync::Arc;

use eos_state::{AgentRunStore, RequestStore, TaskStore};

/// Read-side store handles exposed to the backend composition root.
///
/// Cheap to clone (every field is `Arc`-backed). Construct it through
/// [`RuntimeServices::state_reader`](super::RuntimeServices::state_reader).
#[derive(Clone)]
pub struct StateReader {
    requests: Arc<dyn RequestStore>,
    tasks: Arc<dyn TaskStore>,
    agent_runs: Arc<dyn AgentRunStore>,
}

impl StateReader {
    pub(crate) fn new(
        requests: Arc<dyn RequestStore>,
        tasks: Arc<dyn TaskStore>,
        agent_runs: Arc<dyn AgentRunStore>,
    ) -> Self {
        Self {
            requests,
            tasks,
            agent_runs,
        }
    }

    /// The request store (`list` / `get` / `finish_request`).
    #[must_use]
    pub fn requests(&self) -> Arc<dyn RequestStore> {
        self.requests.clone()
    }

    /// The task store (`list_for_request` / `get`).
    #[must_use]
    pub fn tasks(&self) -> Arc<dyn TaskStore> {
        self.tasks.clone()
    }

    /// The agent-run store (`get_for_task` / `get`).
    #[must_use]
    pub fn agent_runs(&self) -> Arc<dyn AgentRunStore> {
        self.agent_runs.clone()
    }
}

impl std::fmt::Debug for StateReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateReader").finish_non_exhaustive()
    }
}
