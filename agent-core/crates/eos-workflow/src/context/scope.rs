use eos_types::{AttemptId, IterationId, TaskId, WorkItemId, WorkflowId};

use super::ContextRole;

/// Role-specific launch scope. Each role carries only the ids it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextScope {
    /// Planner launch context.
    Planner {
        /// Workflow id.
        workflow_id: WorkflowId,
        /// Iteration id.
        iteration_id: IterationId,
        /// Attempt id.
        attempt_id: AttemptId,
        /// Opaque planner task id.
        task_id: TaskId,
    },
    /// Worker launch context.
    Worker {
        /// Workflow id.
        workflow_id: WorkflowId,
        /// Iteration id.
        iteration_id: IterationId,
        /// Attempt id.
        attempt_id: AttemptId,
        /// Opaque worker task id.
        task_id: TaskId,
        /// Planner-authored work item id.
        work_item_id: WorkItemId,
    },
}

impl ContextScope {
    /// Scope for a planner launch.
    #[must_use]
    pub fn for_planner(
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
        task_id: TaskId,
    ) -> Self {
        Self::Planner {
            workflow_id,
            iteration_id,
            attempt_id,
            task_id,
        }
    }

    /// Scope for a worker launch.
    #[must_use]
    pub fn for_worker(
        workflow_id: WorkflowId,
        iteration_id: IterationId,
        attempt_id: AttemptId,
        task_id: TaskId,
        work_item_id: WorkItemId,
    ) -> Self {
        Self::Worker {
            workflow_id,
            iteration_id,
            attempt_id,
            task_id,
            work_item_id,
        }
    }

    /// The launch role this scope was built for.
    #[must_use]
    pub const fn role(&self) -> ContextRole {
        match self {
            Self::Planner { .. } => ContextRole::Planner,
            Self::Worker { .. } => ContextRole::Worker,
        }
    }
}
