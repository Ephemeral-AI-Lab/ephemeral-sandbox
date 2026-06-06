//! `EventLogRepo` — the `event_log` repository.
//!
//! The store is dumb persistence: the per-request monotonic `seq` is reserved by
//! the backend event bus (Phase 5) and supplied on the [`EventRecord`]. This repo
//! only appends durable rows and replays them by sequence.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;

use eos_backend_types::EventRecord;
use eos_types::RequestId;

use crate::db::{id_in, json_decode, json_encode, ts_in, ts_out, StoreError};

const COLUMNS: &str = "request_id, seq, kind, payload_json, created_at";

/// Repository for persisted milestone events. Holds a cheap `SqlitePool` clone.
#[derive(Debug, Clone)]
pub struct EventLogRepo {
    pool: SqlitePool,
}

impl EventLogRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Append a durable event row at its caller-reserved `seq`.
    ///
    /// # Errors
    /// [`StoreError`] on a `(request_id, seq)` collision or encode failure.
    pub async fn append(&self, record: &EventRecord) -> Result<(), StoreError> {
        sqlx::query(&format!(
            "INSERT INTO event_log ({COLUMNS}) VALUES (?, ?, ?, ?, ?)"
        ))
        .bind(record.request_id.as_str())
        .bind(record.seq)
        .bind(&record.kind)
        .bind(json_encode(&record.payload)?)
        .bind(ts_in(record.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replay events for a request with `seq > after_seq`, ascending. Pass
    /// `after_seq = 0` to replay from the start (sequences begin at 1).
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn list_since(
        &self,
        request_id: &RequestId,
        after_seq: i64,
    ) -> Result<Vec<EventRecord>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM event_log WHERE request_id = ? AND seq > ? ORDER BY seq ASC"
        ))
        .bind(request_id.as_str())
        .bind(after_seq)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_event).collect()
    }

    /// The highest persisted `seq` for a request, or `None` if it has no events.
    ///
    /// # Errors
    /// [`StoreError`] on a query failure.
    pub async fn max_seq(&self, request_id: &RequestId) -> Result<Option<i64>, StoreError> {
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM event_log WHERE request_id = ?")
            .bind(request_id.as_str())
            .fetch_one(&self.pool)
            .await?;
        Ok(max)
    }
}

fn row_to_event(row: &SqliteRow) -> Result<EventRecord, StoreError> {
    let payload_json: String = row.try_get("payload_json")?;
    let created_at: OffsetDateTime = row.try_get("created_at")?;
    Ok(EventRecord {
        request_id: id_in("event_log.request_id", row.try_get("request_id")?)?,
        seq: row.try_get("seq")?,
        kind: row.try_get("kind")?,
        payload: json_decode(&payload_json)?,
        created_at: ts_out(created_at),
    })
}
