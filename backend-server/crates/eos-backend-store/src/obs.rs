//! `ObsEventRepo` and `SandboxCallCorrelationRepo` — the observability event log
//! and the model/daemon correlation bridge.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;

use eos_backend_types::{ObsEvent, ObsSource, SandboxCallCorrelation};
use eos_protocol::CallerId;
use eos_types::{AgentRunId, InvocationId, RequestId, SandboxId};

use crate::db::{id_in, json_decode, json_encode, opt_id_in, ts_in, ts_out, StoreError};

const OBS_COLUMNS: &str = "id, request_id, task_id, agent_run_id, tool_use_id, \
     sandbox_invocation_id, sandbox_id, source, kind, payload_json, created_at";

const OBS_INSERT_COLUMNS: &str = "request_id, task_id, agent_run_id, tool_use_id, \
     sandbox_invocation_id, sandbox_id, source, kind, payload_json, created_at";

const CORR_COLUMNS: &str = "request_id, task_id, agent_run_id, tool_use_id, \
     sandbox_invocation_id, caller_id, sandbox_id, created_at";

/// Repository for persisted observability events. Holds a cheap `SqlitePool` clone.
#[derive(Debug, Clone)]
pub struct ObsEventRepo {
    pool: SqlitePool,
}

impl ObsEventRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert an obs event and return its autoincrement id. Unmatched daemon
    /// rows are inserted with null model-facing ids (AC7).
    ///
    /// # Errors
    /// [`StoreError`] on a query or encode failure.
    pub async fn insert(&self, event: &ObsEvent) -> Result<i64, StoreError> {
        let result = sqlx::query(&format!(
            "INSERT INTO obs_event ({OBS_INSERT_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(event.request_id.as_ref().map(RequestId::as_str))
        .bind(event.task_id.as_ref().map(|id| id.as_str()))
        .bind(event.agent_run_id.as_ref().map(AgentRunId::as_str))
        .bind(event.tool_use_id.as_ref().map(|id| id.as_str()))
        .bind(event.sandbox_invocation_id.as_ref().map(InvocationId::as_str))
        .bind(event.sandbox_id.as_ref().map(SandboxId::as_str))
        .bind(event.source.as_str())
        .bind(&event.kind)
        .bind(json_encode(&event.payload)?)
        .bind(ts_in(event.created_at))
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// All obs events for a request, oldest-first.
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn list_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<ObsEvent>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {OBS_COLUMNS} FROM obs_event WHERE request_id = ? ORDER BY id ASC"
        ))
        .bind(request_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_obs_event).collect()
    }
}

/// Repository for the model/daemon correlation bridge. Holds a cheap pool clone.
#[derive(Debug, Clone)]
pub struct SandboxCallCorrelationRepo {
    pool: SqlitePool,
}

impl SandboxCallCorrelationRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a correlation bridge row (recorded before the daemon request is
    /// sent). Keyed by `(sandbox_id, caller_id, sandbox_invocation_id)`.
    ///
    /// # Errors
    /// [`StoreError`] on a primary-key collision or query failure.
    pub async fn insert(&self, bridge: &SandboxCallCorrelation) -> Result<(), StoreError> {
        sqlx::query(&format!(
            "INSERT INTO sandbox_call_correlation ({CORR_COLUMNS}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(bridge.request_id.as_str())
        .bind(bridge.task_id.as_str())
        .bind(bridge.agent_run_id.as_str())
        .bind(bridge.tool_use_id.as_str())
        .bind(bridge.sandbox_invocation_id.as_str())
        .bind(bridge.caller_id.0.as_str())
        .bind(bridge.sandbox_id.as_str())
        .bind(ts_in(bridge.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Look up a bridge row by its full daemon join key.
    ///
    /// # Errors
    /// [`StoreError`] on a query or decode failure.
    pub async fn get(
        &self,
        sandbox_id: &SandboxId,
        caller_id: &CallerId,
        sandbox_invocation_id: &InvocationId,
    ) -> Result<Option<SandboxCallCorrelation>, StoreError> {
        let row = sqlx::query(&format!(
            "SELECT {CORR_COLUMNS} FROM sandbox_call_correlation \
             WHERE sandbox_id = ? AND caller_id = ? AND sandbox_invocation_id = ?"
        ))
        .bind(sandbox_id.as_str())
        .bind(caller_id.0.as_str())
        .bind(sandbox_invocation_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_correlation).transpose()
    }
}

fn row_to_obs_event(row: &SqliteRow) -> Result<ObsEvent, StoreError> {
    let source_raw: String = row.try_get("source")?;
    let source = ObsSource::from_db(&source_raw).ok_or(StoreError::InvalidEnum {
        field: "obs_event.source",
        value: source_raw,
    })?;
    let payload_json: String = row.try_get("payload_json")?;
    let created_at: OffsetDateTime = row.try_get("created_at")?;
    Ok(ObsEvent {
        id: Some(row.try_get("id")?),
        request_id: opt_id_in("obs_event.request_id", row.try_get("request_id")?)?,
        task_id: opt_id_in("obs_event.task_id", row.try_get("task_id")?)?,
        agent_run_id: opt_id_in("obs_event.agent_run_id", row.try_get("agent_run_id")?)?,
        tool_use_id: opt_id_in("obs_event.tool_use_id", row.try_get("tool_use_id")?)?,
        sandbox_invocation_id: opt_id_in(
            "obs_event.sandbox_invocation_id",
            row.try_get("sandbox_invocation_id")?,
        )?,
        sandbox_id: opt_id_in("obs_event.sandbox_id", row.try_get("sandbox_id")?)?,
        source,
        kind: row.try_get("kind")?,
        payload: json_decode(&payload_json)?,
        created_at: ts_out(created_at),
    })
}

fn row_to_correlation(row: &SqliteRow) -> Result<SandboxCallCorrelation, StoreError> {
    let created_at: OffsetDateTime = row.try_get("created_at")?;
    Ok(SandboxCallCorrelation {
        request_id: id_in("sandbox_call_correlation.request_id", row.try_get("request_id")?)?,
        task_id: id_in("sandbox_call_correlation.task_id", row.try_get("task_id")?)?,
        agent_run_id: id_in(
            "sandbox_call_correlation.agent_run_id",
            row.try_get("agent_run_id")?,
        )?,
        tool_use_id: id_in("sandbox_call_correlation.tool_use_id", row.try_get("tool_use_id")?)?,
        sandbox_invocation_id: id_in(
            "sandbox_call_correlation.sandbox_invocation_id",
            row.try_get("sandbox_invocation_id")?,
        )?,
        caller_id: CallerId(row.try_get("caller_id")?),
        sandbox_id: id_in("sandbox_call_correlation.sandbox_id", row.try_get("sandbox_id")?)?,
        created_at: ts_out(created_at),
    })
}
