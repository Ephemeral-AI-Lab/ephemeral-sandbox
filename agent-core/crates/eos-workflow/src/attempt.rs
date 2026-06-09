mod launch;
mod orchestrator;
mod orchestrator_registry {
    use std::collections::HashMap;
    use std::sync::Arc;

    use eos_types::AttemptId;
    use parking_lot::Mutex;

    use crate::{Result, WorkflowError};

    use super::AttemptOrchestrator;

    /// Process-local liveness map for active attempt orchestrators.
    #[derive(Default)]
    pub struct AttemptOrchestratorRegistry {
        by_attempt_id: Mutex<HashMap<AttemptId, Arc<AttemptOrchestrator>>>,
    }

    impl std::fmt::Debug for AttemptOrchestratorRegistry {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AttemptOrchestratorRegistry")
                .field("len", &self.by_attempt_id.lock().len())
                .finish()
        }
    }

    impl AttemptOrchestratorRegistry {
        /// Create an empty registry.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        pub(crate) fn register(&self, orchestrator: Arc<AttemptOrchestrator>) -> Result<()> {
            let mut guard = self.by_attempt_id.lock();
            let attempt_id = orchestrator.attempt_id().clone();
            if let Some(current) = guard.get(&attempt_id) {
                if !Arc::ptr_eq(current, &orchestrator) {
                    return Err(WorkflowError::invariant(format!(
                        "attempt orchestrator already registered for attempt {:?}",
                        attempt_id.as_str()
                    )));
                }
            }
            guard.insert(attempt_id, orchestrator);
            Ok(())
        }

        /// Look up an active orchestrator.
        #[must_use]
        pub fn get(&self, attempt_id: &AttemptId) -> Option<Arc<AttemptOrchestrator>> {
            self.by_attempt_id.lock().get(attempt_id).cloned()
        }

        pub(crate) fn deregister(&self, attempt_id: &AttemptId) {
            self.by_attempt_id.lock().remove(attempt_id);
        }
    }
}
mod plan_dag;
mod run_stage;

pub use launch::{
    AgentLaunch, AgentLaunchFactory, AgentRunReport, AgentRunner, AttemptResources,
    GeneratorLaunch, PlannerLaunch, ReducerLaunch,
};
pub use orchestrator::AttemptOrchestrator;
pub use orchestrator_registry::AttemptOrchestratorRegistry;
pub use run_stage::AttemptStageAdvancer;
