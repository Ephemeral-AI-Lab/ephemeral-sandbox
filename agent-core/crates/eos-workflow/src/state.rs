//! Workflow behavior over shared state contracts.

mod projections {
    //! Pure workflow outcome-projection algebra.

    use eos_types::{
        Attempt, AttemptStatus, CoreError, ExecutionRole, ExecutionTaskOutcome, TaskOutcomeStatus,
        TaskStore,
    };

    /// Project generator/reducer execution outcomes for one attempt.
    ///
    /// # Errors
    /// Propagates any [`TaskStore::get`] failure.
    pub(crate) async fn project_attempt_outcomes(
        attempt: &Attempt,
        task_store: Option<&dyn TaskStore>,
    ) -> Result<Vec<ExecutionTaskOutcome>, CoreError> {
        let Some(store) = task_store else {
            return Ok(attempt.outcomes().to_vec());
        };
        let mut out: Vec<ExecutionTaskOutcome> = Vec::new();
        for task_id in attempt
            .generator_task_ids()
            .iter()
            .chain(attempt.reducer_task_ids().iter())
        {
            if let Some(task) = store.get(task_id).await? {
                out.extend(task.outcomes.iter().cloned());
            }
        }
        Ok(out)
    }

    /// Persisted attempt outcomes when present, else recompute from task rows.
    ///
    /// # Errors
    /// Propagates any [`TaskStore::get`] failure.
    pub(crate) async fn attempt_execution_outcomes(
        attempt: &Attempt,
        task_store: Option<&dyn TaskStore>,
    ) -> Result<Vec<ExecutionTaskOutcome>, CoreError> {
        if !attempt.outcomes().is_empty() {
            return Ok(attempt.outcomes().to_vec());
        }
        project_attempt_outcomes(attempt, task_store).await
    }

    /// Execution evidence for the iteration's closing attempt only.
    ///
    /// # Errors
    /// Propagates any [`TaskStore::get`] failure.
    pub(crate) async fn project_iteration_outcomes(
        attempts: &[Attempt],
        task_store: Option<&dyn TaskStore>,
    ) -> Result<Vec<ExecutionTaskOutcome>, CoreError> {
        let Some(final_attempt) = attempts.last() else {
            return Ok(Vec::new());
        };
        let final_outcomes = attempt_execution_outcomes(final_attempt, task_store).await?;
        let filtered = if final_attempt.status() == AttemptStatus::Passed {
            final_outcomes
                .into_iter()
                .filter(|o| {
                    o.role == ExecutionRole::Reducer && o.status == TaskOutcomeStatus::Success
                })
                .collect()
        } else {
            final_outcomes
                .into_iter()
                .filter(|o| {
                    matches!(o.role, ExecutionRole::Generator | ExecutionRole::Reducer)
                        && o.status == TaskOutcomeStatus::Failed
                })
                .collect()
        };
        Ok(filtered)
    }
}

pub(crate) use projections::{
    attempt_execution_outcomes, project_attempt_outcomes, project_iteration_outcomes,
};
