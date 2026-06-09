use std::sync::Arc;

use eos_types::{
    Attempt, AttemptClosure, AttemptFailReason, AttemptId, AttemptStage, PlanOutcomeSubmission,
    WorkerOutcomeSubmission,
};

use crate::{Result, WorkflowError};

use super::planner_run::PlannerRun;
use super::work_items_run::WorkItemsRun;
use super::AttemptResources;

/// State machine for one attempt.
pub struct AttemptRun {
    attempt_id: AttemptId,
    deps: AttemptResources,
}

impl std::fmt::Debug for AttemptRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptRun")
            .field("attempt_id", &self.attempt_id)
            .finish()
    }
}

impl AttemptRun {
    /// Create an attempt run handle.
    #[must_use]
    pub fn new(attempt: &Attempt, deps: AttemptResources) -> Arc<Self> {
        Arc::new(Self {
            attempt_id: attempt.id.clone(),
            deps,
        })
    }

    /// Attempt id.
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    /// Start the planner run.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        PlannerRun::new(Arc::clone(self)).start().await
    }

    /// Record a planner terminal plan.
    pub(crate) async fn record_plan_outcome(
        self: &Arc<Self>,
        submission: PlanOutcomeSubmission,
    ) -> Result<()> {
        PlannerRun::new(Arc::clone(self))
            .record_plan_outcome(submission)
            .await
    }

    /// Record a worker terminal outcome.
    pub(crate) async fn record_worker_outcome(
        self: &Arc<Self>,
        submission: WorkerOutcomeSubmission,
    ) -> Result<()> {
        WorkItemsRun::new(Arc::clone(self))
            .record_worker_outcome(submission)
            .await
    }

    pub(crate) async fn close_attempt_passed(&self) -> Result<()> {
        self.close_attempt(AttemptClosure::Passed {
            closed_at: eos_types::UtcDateTime::now(),
        })
        .await
    }

    pub(crate) async fn close_attempt_failed(&self, reason: AttemptFailReason) -> Result<()> {
        self.close_attempt(AttemptClosure::Failed {
            reason,
            closed_at: eos_types::UtcDateTime::now(),
        })
        .await
    }

    async fn close_attempt(&self, closure: AttemptClosure) -> Result<()> {
        let attempt = self.fresh_attempt().await?;
        if attempt.is_closed() {
            return Ok(());
        }
        let closed = self.deps.attempt_store.close(&attempt.id, closure).await?;
        self.deps.active_attempt_runs.deregister(&attempt.id);
        if let Some(registry) = &self.deps.iteration_coordinators {
            if let Some(coordinator) = registry.get(&closed.iteration_id) {
                coordinator.handle_attempt_closed(&closed.id).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn fresh_attempt(&self) -> Result<Attempt> {
        self.deps
            .attempt_store
            .get(&self.attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", self.attempt_id.as_str()))
    }

    pub(crate) async fn assert_stage(&self, expected: AttemptStage) -> Result<Attempt> {
        let attempt = self.fresh_attempt().await?;
        if attempt.is_closed() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} is already closed",
                attempt.id.as_str()
            )));
        }
        if attempt.stage() != expected {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} expected stage {:?}, got {:?}",
                attempt.id.as_str(),
                expected,
                attempt.stage()
            )));
        }
        Ok(attempt)
    }

    pub(crate) fn assert_submission_attempt(&self, attempt_id: &AttemptId) -> Result<()> {
        if attempt_id != &self.attempt_id {
            return Err(WorkflowError::invariant(format!(
                "submission attempt {:?} does not match active attempt {:?}",
                attempt_id.as_str(),
                self.attempt_id.as_str()
            )));
        }
        Ok(())
    }

    pub(crate) fn deps(&self) -> &AttemptResources {
        &self.deps
    }

    pub(crate) fn validate_run_concurrency(&self) -> Result<()> {
        if self.deps.max_concurrent_task_runs == 0 {
            return Err(WorkflowError::invariant(
                "max_concurrent_task_runs must be at least 1",
            ));
        }
        Ok(())
    }
}
