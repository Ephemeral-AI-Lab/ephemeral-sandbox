//! `SqlRequestTaskStore` — the requests + tasks repository (Python `task_store.py`).

use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool};
use time::OffsetDateTime;

use eos_state::{
    AttemptId, CoreError, ExecutionTaskOutcome, IterationId, JsonObject, Page, PageResult, Request,
    RequestId, RequestListFilter, RequestStatus, RequestStore, SandboxId, Sealed, Task, TaskId,
    TaskStatus, TaskStore, WorkflowId,
};

use crate::error::DbError;
use crate::json_col;
use crate::rows::{enum_to_db, parse_enum, row_to_request, row_to_task, RequestRow, TaskRow};

/// Shared UPDATE for both task-status setters. `COALESCE` keeps the existing
/// column when the caller passes a NULL `outcomes`/`terminal_tool_result` bind.
const UPDATE_TASK_STATUS_SQL: &str = "UPDATE tasks SET status = ?, \
       outcomes = COALESCE(?, outcomes), \
       terminal_tool_result = COALESCE(?, terminal_tool_result), \
       updated_at = ? WHERE id = ? RETURNING *";

/// `SQLite` repository for requests and tasks. Holds a cheap `SqlitePool` clone.
#[derive(Debug)]
pub struct SqlRequestTaskStore {
    pool: SqlitePool,
}

impl SqlRequestTaskStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Sealed for SqlRequestTaskStore {}

#[async_trait]
impl RequestStore for SqlRequestTaskStore {
    async fn create_request(
        &self,
        request_id: &RequestId,
        cwd: &str,
        sandbox_id: Option<&SandboxId>,
        request_prompt: &str,
    ) -> Result<(), CoreError> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "INSERT INTO requests \
             (id, cwd, sandbox_id, request_prompt, root_task_id, status, created_at, updated_at, finished_at) \
             VALUES (?, ?, ?, ?, NULL, ?, ?, ?, NULL)",
        )
        .bind(request_id.as_str())
        .bind(cwd)
        .bind(sandbox_id.map(SandboxId::as_str))
        .bind(request_prompt)
        .bind(enum_to_db(&RequestStatus::Running))
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(())
    }

    async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError> {
        let row = sqlx::query_as::<Sqlite, RequestRow>("SELECT * FROM requests WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(row.map(row_to_request).transpose()?)
    }

    async fn set_root_task_id(
        &self,
        id: &RequestId,
        root_task_id: &TaskId,
    ) -> Result<Request, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, RequestRow>(
            "UPDATE requests SET root_task_id = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(root_task_id.as_str())
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        let row = row.ok_or_else(|| DbError::NotFound {
            table: "requests",
            id: id.to_string(),
        })?;
        Ok(row_to_request(row)?)
    }

    async fn finish_request(
        &self,
        id: &RequestId,
        status: RequestStatus,
    ) -> Result<Option<Request>, CoreError> {
        let mut tx = self.pool.begin().await.map_err(DbError::from)?;
        let existing = sqlx::query_as::<Sqlite, RequestRow>("SELECT * FROM requests WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(DbError::from)?;
        let Some(row) = existing else {
            return Ok(None);
        };
        // Idempotent on a terminal request: return it unchanged (task_store.py:142).
        if parse_enum::<RequestStatus>("requests.status", &row.status)?.is_terminal() {
            return Ok(Some(row_to_request(row)?));
        }
        let now = OffsetDateTime::now_utc();
        let updated = sqlx::query_as::<Sqlite, RequestRow>(
            "UPDATE requests SET status = ?, finished_at = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(enum_to_db(&status))
        .bind(now)
        .bind(now)
        .bind(id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(DbError::from)?;
        tx.commit().await.map_err(DbError::from)?;
        Ok(Some(row_to_request(updated)?))
    }

    async fn list(
        &self,
        filter: RequestListFilter,
        page: Page,
    ) -> Result<PageResult<Request>, CoreError> {
        // Bind the optional status string twice so `(? IS NULL OR status = ?)`
        // degrades to "no filter" when absent without numbered placeholders.
        let RequestListFilter { status } = filter;
        let status = status.map(|s| enum_to_db(&s));
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE (? IS NULL OR status = ?)")
                .bind(status.as_deref())
                .bind(status.as_deref())
                .fetch_one(&self.pool)
                .await
                .map_err(DbError::from)?;
        let rows = sqlx::query_as::<Sqlite, RequestRow>(
            "SELECT * FROM requests WHERE (? IS NULL OR status = ?) \
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(status.as_deref())
        .bind(status.as_deref())
        .bind(i64::from(page.limit))
        .bind(i64::from(page.offset))
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        let items = rows
            .into_iter()
            .map(row_to_request)
            .collect::<Result<Vec<_>, DbError>>()?;
        Ok(PageResult {
            items,
            total: u64::try_from(total).unwrap_or(0),
        })
    }
}

#[async_trait]
impl TaskStore for SqlRequestTaskStore {
    async fn upsert_task(&self, task: &Task) -> Result<(), CoreError> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "INSERT INTO tasks \
             (id, request_id, role, instruction, status, workflow_id, iteration_id, attempt_id, \
              agent_name, needs, outcomes, terminal_tool_result, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
               request_id = excluded.request_id, role = excluded.role, \
               instruction = excluded.instruction, status = excluded.status, \
               workflow_id = excluded.workflow_id, iteration_id = excluded.iteration_id, \
               attempt_id = excluded.attempt_id, agent_name = excluded.agent_name, \
               needs = excluded.needs, outcomes = excluded.outcomes, \
               terminal_tool_result = excluded.terminal_tool_result, updated_at = excluded.updated_at",
        )
        .bind(task.id.as_str())
        .bind(task.request_id.as_str())
        .bind(enum_to_db(&task.role))
        .bind(&task.instruction)
        .bind(enum_to_db(&task.status))
        .bind(task.workflow_id.as_ref().map(WorkflowId::as_str))
        .bind(task.iteration_id.as_ref().map(IterationId::as_str))
        .bind(task.attempt_id.as_ref().map(AttemptId::as_str))
        .bind(task.agent_name.as_deref())
        .bind(json_col::encode(&task.needs)?)
        .bind(json_col::encode(&task.outcomes)?)
        .bind(task.terminal_tool_result.as_ref().map(json_col::encode).transpose()?)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError> {
        let row = sqlx::query_as::<Sqlite, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(row.map(row_to_task).transpose()?)
    }

    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, CoreError> {
        let now = OffsetDateTime::now_utc();
        let outcomes_json = outcomes.map(json_col::encode).transpose()?;
        let ttr_json = terminal_tool_result.map(json_col::encode).transpose()?;
        let row = sqlx::query_as::<Sqlite, TaskRow>(UPDATE_TASK_STATUS_SQL)
            .bind(enum_to_db(&status))
            .bind(outcomes_json)
            .bind(ttr_json)
            .bind(now)
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        let row = row.ok_or_else(|| DbError::NotFound {
            table: "tasks",
            id: id.to_string(),
        })?;
        Ok(row_to_task(row)?)
    }

    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError> {
        let mut tx = self.pool.begin().await.map_err(DbError::from)?;
        let current = sqlx::query_as::<Sqlite, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(DbError::from)?;
        let Some(current) = current else {
            return Err(DbError::NotFound {
                table: "tasks",
                id: id.to_string(),
            }
            .into());
        };
        if current.status != enum_to_db(&expected) {
            return Ok(None);
        }
        let now = OffsetDateTime::now_utc();
        let outcomes_json = outcomes.map(json_col::encode).transpose()?;
        let ttr_json = terminal_tool_result.map(json_col::encode).transpose()?;
        let updated = sqlx::query_as::<Sqlite, TaskRow>(UPDATE_TASK_STATUS_SQL)
            .bind(enum_to_db(&status))
            .bind(outcomes_json)
            .bind(ttr_json)
            .bind(now)
            .bind(id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(DbError::from)?;
        tx.commit().await.map_err(DbError::from)?;
        Ok(Some(row_to_task(updated)?))
    }

    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, TaskRow>(
            "SELECT * FROM tasks WHERE request_id = ? ORDER BY created_at ASC, id ASC",
        )
        .bind(request_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(rows
            .into_iter()
            .map(row_to_task)
            .collect::<Result<Vec<_>, DbError>>()?)
    }
}
