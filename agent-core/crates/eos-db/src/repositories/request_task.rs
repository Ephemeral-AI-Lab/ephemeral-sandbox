//! `SqlRequestTaskStore` — the requests + tasks repository (Rust `task_store.py`).

use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool};
use time::OffsetDateTime;

use eos_types::{
    CoreError, Request, RequestId, RequestStatus, RequestStore, SandboxId, Sealed, Task, TaskId,
    TaskOutcome, TaskStatus, TaskStore,
};

use crate::error::DbError;
use crate::json_col;
use crate::rows::{enum_to_db, parse_enum, row_to_request, row_to_task, RequestRow, TaskRow};

/// SQL-level compare-and-swap for optimistic task lifecycle transitions.
const UPDATE_TASK_STATUS_IF_CURRENT_SQL: &str = "UPDATE tasks SET status = ?, \
       task_outcome = COALESCE(?, task_outcome), \
       updated_at = ? WHERE id = ? AND status = ? RETURNING *";

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

    async fn list(&self) -> Result<Vec<Request>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, RequestRow>(
            "SELECT * FROM requests ORDER BY created_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        rows.into_iter()
            .map(row_to_request)
            .collect::<Result<Vec<_>, DbError>>()
            .map_err(CoreError::from)
    }
}

#[async_trait]
impl TaskStore for SqlRequestTaskStore {
    async fn insert_task(&self, task: &Task) -> Result<(), CoreError> {
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "INSERT INTO tasks \
             (id, request_id, role, instruction, status, agent_name, task_outcome, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id.as_str())
        .bind(task.request_id.as_str())
        .bind(enum_to_db(&task.role))
        .bind(&task.instruction)
        .bind(enum_to_db(&task.status))
        .bind(task.agent_name.as_deref())
        .bind(
            task.task_outcome
                .as_ref()
                .map(json_col::encode)
                .transpose()?,
        )
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

    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        task_outcome: Option<&TaskOutcome>,
    ) -> Result<Option<Task>, CoreError> {
        let now = OffsetDateTime::now_utc();
        let outcome_json = task_outcome.map(json_col::encode).transpose()?;
        let updated = sqlx::query_as::<Sqlite, TaskRow>(UPDATE_TASK_STATUS_IF_CURRENT_SQL)
            .bind(enum_to_db(&status))
            .bind(outcome_json)
            .bind(now)
            .bind(id.as_str())
            .bind(enum_to_db(&expected))
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        let Some(updated) = updated else {
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM tasks WHERE id = ?")
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(DbError::from)?;
            if exists.is_some() {
                return Ok(None);
            }
            return Err(DbError::NotFound {
                table: "tasks",
                id: id.to_string(),
            }
            .into());
        };
        Ok(Some(row_to_task(updated)?))
    }

    async fn latch_attempt_tasks_cancelled(&self, ids: &[TaskId]) -> Result<(), CoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = OffsetDateTime::now_utc();
        let cancelled = enum_to_db(&TaskStatus::Cancelled);
        let task_outcome = json_col::encode(&TaskOutcome::Worker {
            is_pass: false,
            outcome: "cancelled".to_owned(),
        })?;
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE tasks SET status = ?, \
               task_outcome = COALESCE(task_outcome, ?), \
               updated_at = ? \
             WHERE status IN ('pending', 'running') AND id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql)
            .bind(cancelled)
            .bind(task_outcome)
            .bind(now);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query.execute(&self.pool).await.map_err(DbError::from)?;
        Ok(())
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
