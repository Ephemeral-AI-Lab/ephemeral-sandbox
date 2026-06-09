//! Runtime-facing request and task persistence contracts.

use async_trait::async_trait;

use crate::{
    AttemptId, CoreError, ExecutionTaskOutcome, JsonObject, Page, PageResult, Request, RequestId,
    RequestListFilter, RequestStatus, SandboxId, Task, TaskId, TaskStatus,
};

use super::Sealed;

/// Persistence surface for request/task rows.
#[async_trait]
pub trait TaskStore: Sealed + Send + Sync {
    /// Insert a fresh task row.
    async fn insert_task(&self, task: &Task) -> Result<(), CoreError>;

    /// Load a task by id.
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError>;

    /// Optimistic-concurrency status flip.
    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError>;

    /// Bulk-latch attempt task rows to [`TaskStatus::Cancelled`] before runtime
    /// teardown.
    async fn latch_attempt_tasks_cancelled(
        &self,
        attempt_id: &AttemptId,
        ids: &[TaskId],
    ) -> Result<(), CoreError>;

    /// All tasks owned by one request, ordered by creation.
    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError>;
}

/// Persistence surface for top-level requests.
#[async_trait]
pub trait RequestStore: Sealed + Send + Sync {
    /// Create a new request row.
    async fn create_request(
        &self,
        request_id: &RequestId,
        cwd: &str,
        sandbox_id: Option<&SandboxId>,
        request_prompt: &str,
    ) -> Result<(), CoreError>;

    /// Load a request by id.
    async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError>;

    /// Set the root task id and return the updated request.
    async fn set_root_task_id(
        &self,
        id: &RequestId,
        root_task_id: &TaskId,
    ) -> Result<Request, CoreError>;

    /// Finish the request with `status`, stamping `finished_at` server-side.
    async fn finish_request(
        &self,
        id: &RequestId,
        status: RequestStatus,
    ) -> Result<Option<Request>, CoreError>;

    /// List requests matching `filter`, newest first, within the `page` window.
    async fn list(
        &self,
        filter: RequestListFilter,
        page: Page,
    ) -> Result<PageResult<Request>, CoreError>;
}
