//! Persisted store service group.

use std::sync::Arc;

use eos_state::{
    AgentRunStore, AttemptStore, IterationStore, ModelStore, RequestStore, TaskStore, WorkflowStore,
};

/// Runtime access to persisted request/task/workflow state.
#[derive(Clone)]
pub(crate) struct DbStoreService {
    pub(crate) task_store: Arc<dyn TaskStore>,
    pub(crate) request_store: Arc<dyn RequestStore>,
    pub(crate) workflow_store: Arc<dyn WorkflowStore>,
    pub(crate) iteration_store: Arc<dyn IterationStore>,
    pub(crate) attempt_store: Arc<dyn AttemptStore>,
    pub(crate) agent_run_store: Arc<dyn AgentRunStore>,
    pub(crate) model_store: Arc<dyn ModelStore>,
}

impl std::fmt::Debug for DbStoreService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbStoreService").finish_non_exhaustive()
    }
}
