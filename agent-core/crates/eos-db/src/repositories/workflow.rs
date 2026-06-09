//! `SqlWorkflowStore` — the workflow repository (Rust `workflow_store.py`).

use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool};
use time::OffsetDateTime;

use eos_types::{
    AgentRunId, CoreError, IterationId, RequestId, Sealed, TaskId, ToolUseId, UtcDateTime,
    Workflow, WorkflowId, WorkflowStatus, WorkflowStore,
};

use crate::error::DbError;
use crate::rows::{enum_to_db, row_to_workflow, WorkflowRow};

/// `SQLite` repository for workflows. Returns frozen `Workflow` DTOs.
#[derive(Debug)]
pub struct SqlWorkflowStore {
    pool: SqlitePool,
}

impl SqlWorkflowStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Sealed for SqlWorkflowStore {}

#[async_trait]
impl WorkflowStore for SqlWorkflowStore {
    async fn insert(
        &self,
        request_id: &RequestId,
        parent_task_id: &TaskId,
        parent_agent_run_id: &AgentRunId,
        tool_use_id: Option<&ToolUseId>,
        workflow_goal: &str,
    ) -> Result<Workflow, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, WorkflowRow>(
            "INSERT INTO workflows \
             (id, request_id, parent_task_id, parent_agent_run_id, tool_use_id, workflow_goal, \
              status, iteration_ids, created_at, updated_at, closed_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'open', '[]', ?, ?, NULL) RETURNING *",
        )
        .bind(WorkflowId::new_v4().as_str())
        .bind(request_id.as_str())
        .bind(parent_task_id.as_str())
        .bind(parent_agent_run_id.as_str())
        .bind(tool_use_id.map(ToolUseId::as_str))
        .bind(workflow_goal)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_workflow(row)?)
    }

    async fn get(&self, id: &WorkflowId) -> Result<Option<Workflow>, CoreError> {
        let row = sqlx::query_as::<Sqlite, WorkflowRow>("SELECT * FROM workflows WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(row.map(row_to_workflow).transpose()?)
    }

    async fn append_iteration_id(
        &self,
        id: &WorkflowId,
        iteration_id: &IterationId,
    ) -> Result<Workflow, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, WorkflowRow>(
            "UPDATE workflows \
             SET iteration_ids = json_insert(COALESCE(iteration_ids, '[]'), '$[#]', ?), \
                 updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(iteration_id.as_str())
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        let row = row.ok_or_else(|| DbError::NotFound {
            table: "workflows",
            id: id.to_string(),
        })?;
        Ok(row_to_workflow(row)?)
    }

    async fn set_status(
        &self,
        id: &WorkflowId,
        status: WorkflowStatus,
        closed_at: Option<UtcDateTime>,
    ) -> Result<Workflow, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, WorkflowRow>(
            "UPDATE workflows SET status = ?, \
               closed_at = COALESCE(?, closed_at), \
               updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(enum_to_db(&status))
        .bind(closed_at.map(UtcDateTime::into_inner))
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        let row = row.ok_or_else(|| DbError::NotFound {
            table: "workflows",
            id: id.to_string(),
        })?;
        Ok(row_to_workflow(row)?)
    }

    async fn list_for_parent_task(
        &self,
        parent_task_id: &TaskId,
    ) -> Result<Vec<Workflow>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, WorkflowRow>(
            "SELECT * FROM workflows WHERE parent_task_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_task_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(rows
            .into_iter()
            .map(row_to_workflow)
            .collect::<Result<Vec<_>, _>>()?)
    }

    async fn list_for_launching_agent_run(
        &self,
        parent_agent_run_id: &AgentRunId,
    ) -> Result<Vec<Workflow>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, WorkflowRow>(
            "SELECT * FROM workflows WHERE parent_agent_run_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_agent_run_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(rows
            .into_iter()
            .map(row_to_workflow)
            .collect::<Result<Vec<_>, _>>()?)
    }

    async fn cancel_open_workflows_for_request(
        &self,
        request_id: &RequestId,
        _reason: &str,
    ) -> Result<usize, CoreError> {
        let now = OffsetDateTime::now_utc();
        let updated = sqlx::query(
            "UPDATE workflows SET status = 'cancelled', \
             closed_at = COALESCE(closed_at, ?), updated_at = ? \
             WHERE request_id = ? AND status = 'open'",
        )
        .bind(now)
        .bind(now)
        .bind(request_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(updated.rows_affected() as usize)
    }
}
