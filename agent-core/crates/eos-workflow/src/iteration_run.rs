use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use eos_types::{
    Attempt, AttemptClosure, AttemptFailReason, AttemptId, AttemptStatus, DeferredGoal,
    IterationId, IterationStatus,
};

use crate::attempt::{AttemptResources, AttemptRun};
use crate::{Result, WorkflowError};

/// Iteration close signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IterationRunClosed {
    /// Iteration id.
    pub iteration_id: IterationId,
    /// Lifecycle outcome.
    pub outcome: IterationRunOutcome,
}

/// Typed lifecycle signal for a closed iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IterationRunOutcome {
    /// Workflow is complete.
    Complete,
    /// Continue with a planner-authored deferred goal.
    Continue(DeferredGoal),
    /// Workflow should fail.
    Failed,
}

/// Async callback invoked when an iteration closes.
pub(crate) type IterationClosedCallback = Arc<
    dyn Fn(IterationRunClosed) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync,
>;

/// Coordinates attempts for one open iteration.
pub struct IterationRunCoordinator {
    iteration_id: IterationId,
    deps: AttemptResources,
    on_iteration_closed: IterationClosedCallback,
}

impl std::fmt::Debug for IterationRunCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IterationRunCoordinator")
            .field("iteration_id", &self.iteration_id)
            .finish()
    }
}

impl IterationRunCoordinator {
    /// Create a coordinator.
    #[must_use]
    pub(crate) fn new(
        iteration_id: IterationId,
        deps: AttemptResources,
        on_iteration_closed: IterationClosedCallback,
    ) -> Arc<Self> {
        Arc::new(Self {
            iteration_id,
            deps,
            on_iteration_closed,
        })
    }

    /// Iteration id.
    #[must_use]
    pub fn iteration_id(&self) -> &IterationId {
        &self.iteration_id
    }

    /// Create an attempt.
    pub(crate) async fn create_attempt(
        &self,
        previous_attempt_id: Option<&AttemptId>,
        start: bool,
    ) -> Result<Attempt> {
        let iteration = self.current_iteration_snapshot().await?;
        if !iteration.is_open() {
            return Err(WorkflowError::invariant(format!(
                "iteration {:?} is not open",
                iteration.id.as_str()
            )));
        }
        let sequence_no = if let Some(previous_id) = previous_attempt_id {
            if !iteration.has_budget_remaining() {
                return Err(WorkflowError::invariant(format!(
                    "iteration {:?} attempt budget exhausted",
                    iteration.id.as_str()
                )));
            }
            if iteration.latest_attempt_id() != Some(previous_id) {
                return Err(WorkflowError::invariant(format!(
                    "previous_attempt_id {:?} is not the latest attempt of iteration {:?}",
                    previous_id.as_str(),
                    iteration.id.as_str()
                )));
            }
            iteration.attempt_count() as i64 + 1
        } else {
            if !iteration.attempt_ids.is_empty() {
                return Err(WorkflowError::invariant(format!(
                    "iteration {:?} already has attempts; pass previous_attempt_id to retry",
                    iteration.id.as_str()
                )));
            }
            1
        };
        let attempt = self
            .deps
            .attempt_store
            .insert(&iteration.id, &iteration.workflow_id, sequence_no)
            .await?;
        self.deps
            .iteration_store
            .append_attempt_id(&iteration.id, &attempt.id)
            .await?;
        if start {
            self.start_attempt(&attempt).await?;
        }
        Ok(attempt)
    }

    /// Create and start the first attempt.
    pub(crate) async fn create_and_start_first_attempt(&self) -> Result<Attempt> {
        self.create_attempt(None, true).await
    }

    /// Start an existing attempt.
    pub(crate) async fn start_attempt(&self, attempt: &Attempt) -> Result<()> {
        let iteration = self.current_iteration_snapshot().await?;
        if !iteration.is_open() {
            return Err(WorkflowError::invariant(format!(
                "iteration {:?} is not open",
                iteration.id.as_str()
            )));
        }
        if attempt.iteration_id != iteration.id {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} does not belong to iteration {:?}",
                attempt.id.as_str(),
                iteration.id.as_str()
            )));
        }
        let run = AttemptRun::new(attempt, self.deps.clone());
        if let Err(err) = run.start().await {
            self.close_attempt_after_startup_failure(attempt).await?;
            return Err(err);
        }
        Ok(())
    }

    /// Handle a closed attempt.
    pub(crate) async fn handle_attempt_closed(&self, attempt_id: &AttemptId) -> Result<()> {
        let attempt = self
            .deps
            .attempt_store
            .get(attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", attempt_id.as_str()))?;
        let iteration = self.current_iteration_snapshot().await?;
        if !iteration.is_open() {
            return Ok(());
        }
        if attempt.iteration_id != iteration.id {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} does not belong to iteration {:?}",
                attempt.id.as_str(),
                iteration.id.as_str()
            )));
        }
        if attempt.status() == AttemptStatus::Passed {
            self.close_iteration_passed(&attempt).await
        } else {
            self.retry_or_close_failed(attempt).await
        }
    }

    async fn current_iteration_snapshot(&self) -> Result<eos_types::Iteration> {
        self.deps
            .iteration_store
            .get(&self.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", self.iteration_id.as_str()))
    }

    async fn close_iteration_passed(&self, attempt: &Attempt) -> Result<()> {
        self.deps
            .iteration_store
            .set_status(
                &self.iteration_id,
                IterationStatus::Succeeded,
                Some(eos_types::UtcDateTime::now()),
            )
            .await?;
        let outcome = if let Some(deferred_goal) =
            returned_attempt_deferred_goal(&self.deps, attempt).await?
        {
            IterationRunOutcome::Continue(deferred_goal)
        } else {
            IterationRunOutcome::Complete
        };
        (self.on_iteration_closed)(IterationRunClosed {
            iteration_id: self.iteration_id.clone(),
            outcome,
        })
        .await
    }

    async fn retry_or_close_failed(&self, mut attempt: Attempt) -> Result<()> {
        loop {
            let iteration = self.current_iteration_snapshot().await?;
            if !iteration.has_budget_remaining() {
                return self.close_iteration_failed().await;
            }
            match self.create_attempt(Some(&attempt.id), true).await {
                Ok(_) => return Ok(()),
                Err(err) => {
                    let latest = self.latest_failed_attempt_after(&attempt.id).await?;
                    let Some(retry_attempt) = latest else {
                        return Err(err);
                    };
                    attempt = retry_attempt;
                }
            }
        }
    }

    async fn close_iteration_failed(&self) -> Result<()> {
        self.deps
            .iteration_store
            .set_status(
                &self.iteration_id,
                IterationStatus::Failed,
                Some(eos_types::UtcDateTime::now()),
            )
            .await?;
        (self.on_iteration_closed)(IterationRunClosed {
            iteration_id: self.iteration_id.clone(),
            outcome: IterationRunOutcome::Failed,
        })
        .await
    }

    async fn latest_failed_attempt_after(
        &self,
        previous_id: &AttemptId,
    ) -> Result<Option<Attempt>> {
        let iteration = self.current_iteration_snapshot().await?;
        let Some(latest_id) = iteration.latest_attempt_id() else {
            return Ok(None);
        };
        if latest_id == previous_id {
            return Ok(None);
        }
        let attempt = self.deps.attempt_store.get(latest_id).await?;
        Ok(attempt.filter(|attempt| attempt.status() == AttemptStatus::Failed))
    }

    async fn close_attempt_after_startup_failure(&self, attempt: &Attempt) -> Result<()> {
        let Some(latest) = self.deps.attempt_store.get(&attempt.id).await? else {
            return Ok(());
        };
        if latest.is_closed() {
            return Ok(());
        }
        self.deps
            .attempt_store
            .close(
                &attempt.id,
                AttemptClosure::Failed {
                    reason: AttemptFailReason::StartupFailed,
                    closed_at: eos_types::UtcDateTime::now(),
                },
            )
            .await?;
        Ok(())
    }
}

pub(crate) async fn returned_attempt_deferred_goal(
    deps: &AttemptResources,
    attempt: &Attempt,
) -> Result<Option<DeferredGoal>> {
    let Some(planner_task_id) = attempt.planner_task_id() else {
        return Ok(None);
    };
    let Some(task) = deps.task_store.get(planner_task_id).await? else {
        return Ok(None);
    };
    let Some(outcome) = task
        .task_outcome
        .and_then(|outcome| outcome.planner_outcome())
    else {
        return Ok(None);
    };
    Ok(outcome.deferred_goal_for_next_iteration)
}
