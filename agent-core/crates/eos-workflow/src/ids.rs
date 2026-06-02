use eos_state::{AttemptId, TaskId};

use crate::Result;

/// Per-workflow lifecycle knobs injected by `eos-runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowLifecycleConfig {
    /// Attempts allowed per iteration before the iteration closes failed.
    pub default_attempt_budget: i64,
}

impl Default for WorkflowLifecycleConfig {
    fn default() -> Self {
        Self {
            default_attempt_budget: 2,
        }
    }
}

/// Stable planner task id for an attempt.
pub fn planner_task_id(attempt_id: &AttemptId) -> Result<TaskId> {
    Ok(format!("{}:planner", attempt_id.as_str()).parse()?)
}

/// Stable generator task id from an attempt id and planner-local id.
pub fn generator_task_id(attempt_id: &AttemptId, local_task_id: &str) -> Result<TaskId> {
    Ok(format!("{}:gen:{local_task_id}", attempt_id.as_str()).parse()?)
}

/// Stable reducer task id from an attempt id and planner-local id.
pub fn reducer_task_id(attempt_id: &AttemptId, local_task_id: &str) -> Result<TaskId> {
    Ok(format!("{}:red:{local_task_id}", attempt_id.as_str()).parse()?)
}
