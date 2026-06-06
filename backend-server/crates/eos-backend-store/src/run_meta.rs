//! `RunMetaRepo` — the `run_meta` repository.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;

use eos_backend_types::{BackendRunStatus, Page, PageResult, RunMeta};
use eos_types::{RequestId, UtcDateTime};

use crate::db::{id_in, json_decode, json_encode, ts_in, ts_out, StoreError};

const COLUMNS: &str =
    "request_id, status, label, client_meta_json, created_at, finished_at, cancel_reason";

/// Repository for backend run lifecycle rows. Holds a cheap `SqlitePool` clone.
#[derive(Debug, Clone)]
pub struct RunMetaRepo {
    pool: SqlitePool,
}

impl RunMetaRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new run metadata row.
    ///
    /// # Errors
    /// [`StoreError`] on a constraint violation or encode failure.
    pub async fn insert(&self, meta: &RunMeta) -> Result<(), StoreError> {
        sqlx::query(&format!(
            "INSERT INTO run_meta ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(meta.request_id.as_str())
        .bind(meta.status.as_str())
        .bind(meta.label.as_deref())
        .bind(json_encode(&meta.client_meta)?)
        .bind(ts_in(meta.created_at))
        .bind(meta.finished_at.map(ts_in))
        .bind(meta.cancel_reason.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fetch a run by id.
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn get(&self, request_id: &RequestId) -> Result<Option<RunMeta>, StoreError> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM run_meta WHERE request_id = ?"))
            .bind(request_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_run_meta).transpose()
    }

    /// Set a run's terminal status, `finished_at`, and `cancel_reason`. Returns
    /// the updated row, or `None` if the run is absent.
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn set_status(
        &self,
        request_id: &RequestId,
        status: BackendRunStatus,
        finished_at: Option<UtcDateTime>,
        cancel_reason: Option<&str>,
    ) -> Result<Option<RunMeta>, StoreError> {
        let row = sqlx::query(&format!(
            "UPDATE run_meta SET status = ?, finished_at = ?, cancel_reason = ? \
             WHERE request_id = ? RETURNING {COLUMNS}"
        ))
        .bind(status.as_str())
        .bind(finished_at.map(ts_in))
        .bind(cancel_reason)
        .bind(request_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_run_meta).transpose()
    }

    /// List runs newest-first with limit/offset pagination plus a total count.
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn list(&self, page: Page) -> Result<PageResult<RunMeta>, StoreError> {
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_meta")
            .fetch_one(&self.pool)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM run_meta ORDER BY created_at DESC, request_id DESC \
             LIMIT ? OFFSET ?"
        ))
        .bind(i64::from(page.limit))
        .bind(i64::from(page.offset))
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .iter()
            .map(row_to_run_meta)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PageResult {
            items,
            total: total.max(0) as u64,
            limit: page.limit,
            offset: page.offset,
        })
    }
}

fn row_to_run_meta(row: &SqliteRow) -> Result<RunMeta, StoreError> {
    let status_raw: String = row.try_get("status")?;
    let status = BackendRunStatus::from_db(&status_raw).ok_or(StoreError::InvalidEnum {
        field: "run_meta.status",
        value: status_raw,
    })?;
    let client_meta_json: String = row.try_get("client_meta_json")?;
    let created_at: OffsetDateTime = row.try_get("created_at")?;
    let finished_at: Option<OffsetDateTime> = row.try_get("finished_at")?;
    Ok(RunMeta {
        request_id: id_in("run_meta.request_id", row.try_get("request_id")?)?,
        status,
        label: row.try_get("label")?,
        client_meta: json_decode(&client_meta_json)?,
        created_at: ts_out(created_at),
        finished_at: finished_at.map(ts_out),
        cancel_reason: row.try_get("cancel_reason")?,
    })
}
