//! Recording adapter from terminal submission tools to active attempt runs.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{
    CoreError, PlanOutcomeSubmission, SubmissionAck, WorkerOutcomeSubmission,
    WorkflowAttemptSubmissionApi,
};

use crate::attempt::ActiveAttemptRuns;
use crate::WorkflowError;

/// Recording adapter from terminal contracts to active attempt runs.
#[derive(Clone)]
pub struct AttemptSubmissionAdapter {
    active_attempt_runs: Arc<ActiveAttemptRuns>,
}

impl std::fmt::Debug for AttemptSubmissionAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptSubmissionAdapter")
            .finish_non_exhaustive()
    }
}

impl AttemptSubmissionAdapter {
    /// Create a submission adapter over the active attempt registry.
    #[must_use]
    pub fn new(active_attempt_runs: Arc<ActiveAttemptRuns>) -> Self {
        Self {
            active_attempt_runs,
        }
    }
}

#[async_trait]
impl WorkflowAttemptSubmissionApi for AttemptSubmissionAdapter {
    async fn submit_plan_outcome(
        &self,
        submission: PlanOutcomeSubmission,
    ) -> Result<SubmissionAck, CoreError> {
        let Some(run) = self.active_attempt_runs.get(&submission.attempt_id) else {
            return Ok(SubmissionAck::Rejected(format!(
                "attempt {:?} is not active",
                submission.attempt_id.as_str()
            )));
        };
        submission_ack(run.record_plan_outcome(submission).await)
    }

    async fn submit_worker_outcome(
        &self,
        submission: WorkerOutcomeSubmission,
    ) -> Result<SubmissionAck, CoreError> {
        let Some(run) = self.active_attempt_runs.get(&submission.attempt_id) else {
            return Ok(SubmissionAck::Rejected(format!(
                "attempt {:?} is not active",
                submission.attempt_id.as_str()
            )));
        };
        submission_ack(run.record_worker_outcome(submission).await)
    }
}

fn submission_ack(result: crate::Result<()>) -> Result<SubmissionAck, CoreError> {
    match result {
        Ok(()) => Ok(SubmissionAck::Accepted),
        Err(WorkflowError::Store(err)) => Err(err),
        Err(err) => Ok(SubmissionAck::Rejected(err.to_string())),
    }
}
