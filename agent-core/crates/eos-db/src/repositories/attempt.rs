//! `SqlAttemptStore` — the attempt repository (Rust `attempt_store.py`).

use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool};
use time::OffsetDateTime;

use eos_types::{
    Attempt, AttemptClosure, AttemptExecutionTree, AttemptId, AttemptStage, AttemptStore,
    CoreError, ExecutionNode, IterationId, PlanId, RequestId, Sealed, TaskId, WorkItemId,
    WorkflowId,
};

use crate::error::DbError;
use crate::json_col;
use crate::rows::{enum_to_db, row_to_attempt, AttemptRow};

/// `SQLite` repository for attempts. Returns frozen `Attempt` DTOs.
#[derive(Debug)]
pub struct SqlAttemptStore {
    pool: SqlitePool,
}

impl SqlAttemptStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    fn not_found(id: &AttemptId) -> DbError {
        DbError::NotFound {
            table: "attempts",
            id: id.to_string(),
        }
    }
}

impl Sealed for SqlAttemptStore {}

#[async_trait]
impl AttemptStore for SqlAttemptStore {
    async fn insert(
        &self,
        iteration_id: &IterationId,
        workflow_id: &WorkflowId,
        attempt_sequence_no: i64,
    ) -> Result<Attempt, CoreError> {
        let now = OffsetDateTime::now_utc();
        let plan_id = PlanId::new_v4();
        let execution_tree = AttemptExecutionTree::new(plan_id.clone());
        let row = sqlx::query_as::<Sqlite, AttemptRow>(
            "INSERT INTO attempts \
             (id, iteration_id, workflow_id, attempt_sequence_no, stage, status, plan_id, \
              execution_tree, fail_reason, created_at, updated_at, closed_at) \
             VALUES (?, ?, ?, ?, 'plan', 'running', ?, ?, NULL, ?, ?, NULL) \
             RETURNING *",
        )
        .bind(AttemptId::new_v4().as_str())
        .bind(iteration_id.as_str())
        .bind(workflow_id.as_str())
        .bind(attempt_sequence_no)
        .bind(plan_id.as_str())
        .bind(json_col::encode(&execution_tree)?)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_attempt(row)?)
    }

    async fn get(&self, id: &AttemptId) -> Result<Option<Attempt>, CoreError> {
        let row = sqlx::query_as::<Sqlite, AttemptRow>("SELECT * FROM attempts WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(row.map(row_to_attempt).transpose()?)
    }

    async fn bind_planner_task(
        &self,
        id: &AttemptId,
        planner_task_id: &TaskId,
    ) -> Result<Attempt, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, AttemptRow>(
            "UPDATE attempts \
             SET execution_tree = json_set(execution_tree, '$.planner_task_id', ?), \
                 updated_at = ? \
             WHERE id = ? RETURNING *",
        )
        .bind(planner_task_id.as_str())
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_attempt(row.ok_or_else(|| Self::not_found(id))?)?)
    }

    async fn record_plan_nodes(
        &self,
        id: &AttemptId,
        nodes: &[ExecutionNode],
    ) -> Result<Attempt, CoreError> {
        let now = OffsetDateTime::now_utc();
        let mut attempt = self.get(id).await?.ok_or_else(|| Self::not_found(id))?;
        attempt.execution_tree.nodes = nodes.to_vec();
        let row = sqlx::query_as::<Sqlite, AttemptRow>(
            "UPDATE attempts SET stage = ?, execution_tree = ?, updated_at = ? \
             WHERE id = ? RETURNING *",
        )
        .bind(enum_to_db(&AttemptStage::Run))
        .bind(json_col::encode(&attempt.execution_tree)?)
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_attempt(row.ok_or_else(|| Self::not_found(id))?)?)
    }

    async fn bind_worker_task(
        &self,
        id: &AttemptId,
        work_item_id: &WorkItemId,
        task_id: &TaskId,
    ) -> Result<Attempt, CoreError> {
        let now = OffsetDateTime::now_utc();
        let mut attempt = self.get(id).await?.ok_or_else(|| Self::not_found(id))?;
        let Some(node) = attempt
            .execution_tree
            .nodes
            .iter_mut()
            .find(|node| node.work_item_id == *work_item_id)
        else {
            return Err(CoreError::Store(format!(
                "work item '{}' not found in attempt '{}'",
                work_item_id.as_str(),
                id.as_str()
            )));
        };
        node.task_id = Some(task_id.clone());
        let row = sqlx::query_as::<Sqlite, AttemptRow>(
            "UPDATE attempts SET execution_tree = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(json_col::encode(&attempt.execution_tree)?)
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_attempt(row.ok_or_else(|| Self::not_found(id))?)?)
    }

    async fn close(&self, id: &AttemptId, closure: AttemptClosure) -> Result<Attempt, CoreError> {
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query_as::<Sqlite, AttemptRow>(
            "UPDATE attempts SET stage = 'closed', status = ?, fail_reason = ?, \
               closed_at = ?, updated_at = ? \
             WHERE id = ? RETURNING *",
        )
        .bind(enum_to_db(&closure.status()))
        .bind(closure.fail_reason().as_ref().map(enum_to_db))
        .bind(closure.closed_at().into_inner())
        .bind(now)
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(row_to_attempt(row.ok_or_else(|| Self::not_found(id))?)?)
    }

    async fn list_for_iteration(
        &self,
        iteration_id: &IterationId,
    ) -> Result<Vec<Attempt>, CoreError> {
        let rows = sqlx::query_as::<Sqlite, AttemptRow>(
            "SELECT * FROM attempts WHERE iteration_id = ? ORDER BY attempt_sequence_no ASC",
        )
        .bind(iteration_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(DbError::from)?;
        Ok(rows
            .into_iter()
            .map(row_to_attempt)
            .collect::<Result<Vec<_>, _>>()?)
    }

    async fn cancel_open_attempts_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<usize, CoreError> {
        let now = OffsetDateTime::now_utc();
        let updated = sqlx::query(
            "UPDATE attempts SET stage = 'closed', status = 'cancelled', \
             fail_reason = NULL, closed_at = COALESCE(closed_at, ?), updated_at = ? \
             WHERE status = 'running' AND workflow_id IN \
             (SELECT id FROM workflows WHERE request_id = ?)",
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
