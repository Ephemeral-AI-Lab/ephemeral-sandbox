use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use eos_state::{
    project_iteration_outcomes, Attempt, AttemptFailReason, AttemptId, AttemptStatus, AttemptStore,
    IterationId, IterationStatus,
};
use parking_lot::Mutex;

use crate::attempt::{AttemptDeps, AttemptOrchestrator};
use crate::{Result, WorkflowError};

/// Iteration close signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IterationClosed {
    /// Iteration id.
    pub iteration_id: IterationId,
    /// Whether the iteration succeeded.
    pub succeeded: bool,
    /// Deferred goal, if any.
    pub deferred_goal: Option<String>,
}

/// Async callback invoked when an iteration closes.
pub type IterationClosedCallback =
    Arc<dyn Fn(IterationClosed) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;

/// Coordinates attempts for one open iteration.
pub struct IterationAttemptCoordinator {
    iteration_id: IterationId,
    deps: AttemptDeps,
    on_iteration_closed: IterationClosedCallback,
}

impl std::fmt::Debug for IterationAttemptCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IterationAttemptCoordinator")
            .field("iteration_id", &self.iteration_id)
            .finish()
    }
}

impl IterationAttemptCoordinator {
    /// Create a coordinator.
    #[must_use]
    pub fn new(
        iteration_id: IterationId,
        deps: AttemptDeps,
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
    pub async fn create_attempt(
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
    pub async fn create_and_start_first_attempt(&self) -> Result<Attempt> {
        self.create_attempt(None, true).await
    }

    /// Start an existing attempt.
    pub async fn start_attempt(&self, attempt: &Attempt) -> Result<()> {
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
        let orchestrator = AttemptOrchestrator::new(attempt, self.deps.clone());
        if let Err(err) = orchestrator.start().await {
            self.close_attempt_after_startup_failure(attempt).await?;
            return Err(err);
        }
        Ok(())
    }

    /// Handle a closed attempt.
    pub async fn handle_attempt_closed(&self, attempt_id: &AttemptId) -> Result<()> {
        let attempt = self
            .deps
            .attempt_store
            .get(attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", attempt_id.as_str()))?;
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
        if attempt.status == AttemptStatus::Failed && attempt.fail_reason.is_none() {
            return Err(WorkflowError::invariant(format!(
                "attempt {:?} closed failed with no fail_reason",
                attempt.id.as_str()
            )));
        }
        if attempt.status == AttemptStatus::Passed {
            self.close_iteration_passed(&attempt).await
        } else {
            self.retry_or_close_failed(attempt).await
        }
    }

    async fn current_iteration_snapshot(&self) -> Result<eos_state::Iteration> {
        self.deps
            .iteration_store
            .get(&self.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", self.iteration_id.as_str()))
    }

    async fn close_iteration_passed(&self, attempt: &Attempt) -> Result<()> {
        self.deps
            .iteration_store
            .set_deferred_goal_for_next_iteration(
                &self.iteration_id,
                attempt.deferred_goal_for_next_iteration.as_deref(),
            )
            .await?;
        let outcomes = iteration_outcomes_json(
            self.deps.attempt_store.as_ref(),
            self.deps.task_store.as_ref(),
            attempt,
        )
        .await?;
        self.deps
            .iteration_store
            .close_succeeded(
                &self.iteration_id,
                &outcomes,
                Some(eos_state::UtcDateTime::now()),
            )
            .await?;
        (self.on_iteration_closed)(IterationClosed {
            iteration_id: self.iteration_id.clone(),
            succeeded: true,
            deferred_goal: attempt.deferred_goal_for_next_iteration.clone(),
        })
        .await
    }

    async fn retry_or_close_failed(&self, mut attempt: Attempt) -> Result<()> {
        loop {
            let iteration = self.current_iteration_snapshot().await?;
            if !iteration.has_budget_remaining() {
                return self.close_iteration_failed(&attempt).await;
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

    async fn close_iteration_failed(&self, attempt: &Attempt) -> Result<()> {
        let outcomes = iteration_outcomes_json(
            self.deps.attempt_store.as_ref(),
            self.deps.task_store.as_ref(),
            attempt,
        )
        .await?;
        self.deps
            .iteration_store
            .set_status(
                &self.iteration_id,
                IterationStatus::Failed,
                Some(eos_state::UtcDateTime::now()),
                Some(&outcomes),
            )
            .await?;
        (self.on_iteration_closed)(IterationClosed {
            iteration_id: self.iteration_id.clone(),
            succeeded: false,
            deferred_goal: None,
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
        Ok(attempt.filter(|attempt| attempt.status == AttemptStatus::Failed))
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
                AttemptStatus::Failed,
                Some(AttemptFailReason::StartupFailed),
                None,
                eos_state::UtcDateTime::now(),
            )
            .await?;
        Ok(())
    }
}

async fn iteration_outcomes_json(
    attempt_store: &dyn AttemptStore,
    task_store: &dyn eos_state::TaskStore,
    attempt: &Attempt,
) -> Result<String> {
    let attempts = attempt_store
        .list_for_iteration(&attempt.iteration_id)
        .await?;
    let outcomes = project_iteration_outcomes(&attempts, Some(task_store)).await?;
    Ok(serde_json::to_string(&outcomes)?)
}

/// Process-local one-coordinator-per-open-iteration registry.
#[derive(Default)]
pub struct OpenIterationCoordinatorRegistry {
    by_iteration_id:
        Mutex<std::collections::HashMap<IterationId, Arc<IterationAttemptCoordinator>>>,
}

impl std::fmt::Debug for OpenIterationCoordinatorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenIterationCoordinatorRegistry")
            .field("len", &self.by_iteration_id.lock().len())
            .finish()
    }
}

impl OpenIterationCoordinatorRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a coordinator.
    pub fn register(&self, coordinator: Arc<IterationAttemptCoordinator>) -> Result<()> {
        let mut guard = self.by_iteration_id.lock();
        if guard.contains_key(coordinator.iteration_id()) {
            return Err(WorkflowError::invariant(format!(
                "iteration attempt coordinator already registered for iteration {:?}",
                coordinator.iteration_id().as_str()
            )));
        }
        guard.insert(coordinator.iteration_id().clone(), coordinator);
        Ok(())
    }

    /// Look up a coordinator.
    #[must_use]
    pub fn get(&self, iteration_id: &IterationId) -> Option<Arc<IterationAttemptCoordinator>> {
        self.by_iteration_id.lock().get(iteration_id).cloned()
    }

    /// Deregister a coordinator.
    pub fn deregister(&self, iteration_id: &IterationId) {
        self.by_iteration_id.lock().remove(iteration_id);
    }
}
