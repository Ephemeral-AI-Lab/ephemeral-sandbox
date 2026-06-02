use eos_state::{AttemptId, IterationId, TaskId, WorkflowId};

use crate::{Result, WorkflowError};

use super::ContextRole;

/// Identity fields a context builder can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextScope {
    /// Launch role.
    pub role: ContextRole,
    /// Workflow id.
    pub workflow_id: Option<WorkflowId>,
    /// Iteration id.
    pub iteration_id: Option<IterationId>,
    /// Attempt id.
    pub attempt_id: Option<AttemptId>,
    /// Assigned task id.
    pub task_id: Option<TaskId>,
}

impl ContextScope {
    /// Scope for a planner launch.
    #[must_use]
    pub fn for_planner(
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
    ) -> Self {
        Self {
            role: ContextRole::Planner,
            workflow_id: Some(workflow_id),
            iteration_id: Some(iteration_id),
            attempt_id: Some(attempt_id),
            task_id: None,
        }
    }

    /// Scope for a generator launch.
    #[must_use]
    pub fn for_generator(
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
        task_id: TaskId,
    ) -> Self {
        Self {
            role: ContextRole::Generator,
            workflow_id: Some(workflow_id),
            iteration_id: Some(iteration_id),
            attempt_id: Some(attempt_id),
            task_id: Some(task_id),
        }
    }

    /// Scope for a reducer launch.
    #[must_use]
    pub fn for_reducer(
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
        task_id: TaskId,
    ) -> Self {
        Self {
            role: ContextRole::Reducer,
            workflow_id: Some(workflow_id),
            iteration_id: Some(iteration_id),
            attempt_id: Some(attempt_id),
            task_id: Some(task_id),
        }
    }

    pub(crate) fn workflow_id(&self) -> Result<&WorkflowId> {
        self.workflow_id
            .as_ref()
            .ok_or(WorkflowError::MissingContextField("workflow_id"))
    }

    pub(crate) fn iteration_id(&self) -> Result<&IterationId> {
        self.iteration_id
            .as_ref()
            .ok_or(WorkflowError::MissingContextField("iteration_id"))
    }

    pub(crate) fn attempt_id(&self) -> Result<&AttemptId> {
        self.attempt_id
            .as_ref()
            .ok_or(WorkflowError::MissingContextField("attempt_id"))
    }

    pub(crate) fn task_id(&self) -> Result<&TaskId> {
        self.task_id
            .as_ref()
            .ok_or(WorkflowError::MissingContextField("task_id"))
    }
}
