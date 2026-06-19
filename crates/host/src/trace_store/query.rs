//! Read-side projection/query over the trace store sqlite schema. These free
//! functions operate on a borrowed `Connection` and are delegated to by the
//! public read methods on `TraceStore` so callers keep a single type.

#[cfg(any(test, feature = "e2e-support"))]
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection};

use super::TraceStoreError;

#[cfg(any(test, feature = "e2e-support"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlitePosture {
    pub journal_mode: String,
    pub synchronous: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceEventRow {
    pub seq: i64,
    pub module: String,
    pub event: String,
    pub details_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceRequestRow {
    pub request_id: String,
    pub trace_id: String,
    pub sandbox_id: String,
    pub op: String,
    pub caller_id: Option<String>,
    pub args_summary: Option<String>,
    pub args_digest: Option<String>,
    pub workspace_route: Option<String>,
    pub status: Option<String>,
    pub error_kind: Option<String>,
    pub sent_at_ms: i64,
    pub received_at_ms: Option<i64>,
    pub host_rtt_ms: Option<i64>,
    pub duration_us: Option<i64>,
    pub daemon_boot_id: Option<String>,
    pub host_boot_id: String,
    pub modules_touched: Option<String>,
    pub response_digest: Option<String>,
    pub response_len: Option<i64>,
    pub response_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceSpanRow {
    pub trace_id: String,
    pub request_id: Option<String>,
    pub span_id: i64,
    pub parent_span_id: Option<i64>,
    pub kind: String,
    pub subsystem: String,
    pub status: String,
    pub started_us: i64,
    pub duration_us: i64,
    pub fields_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceResourceRow {
    pub trace_id: String,
    pub request_id: Option<String>,
    pub span_id: Option<i64>,
    pub ts_us: i64,
    pub kind: String,
    pub values_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceLinkRow {
    pub trace_id: String,
    pub link_kind: String,
    pub link_id: String,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceAuditEntryRow {
    pub audit_seq: i64,
    pub sandbox_id: String,
    pub trace_id: String,
    pub request_id: Option<String>,
    pub entry_kind: String,
    pub schema_name: String,
    pub schema_version: i64,
    pub received_at_ms: i64,
    pub payload_sha256: String,
    pub prev_global_sha256: Option<String>,
    pub prev_sandbox_sha256: Option<String>,
    pub entry_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AuditVerificationRow {
    pub audit_seq: i64,
    pub sandbox_id: String,
    pub trace_id: String,
    pub request_id: Option<String>,
    pub entry_kind: String,
    pub schema_name: String,
    pub schema_version: i64,
    pub received_at_ms: i64,
    pub payload: Vec<u8>,
    pub payload_sha256: String,
    pub prev_global_sha256: Option<String>,
    pub prev_sandbox_sha256: Option<String>,
    pub entry_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectionGap {
    pub audit_seq: i64,
    pub request_id: String,
}

const TRACE_REQUEST_COLUMNS: &str = "\
request_id, trace_id, sandbox_id, op, caller_id, args_summary,
args_digest, workspace_route, status, error_kind, sent_at_ms, received_at_ms,
host_rtt_ms, duration_us, daemon_boot_id, host_boot_id, modules_touched,
response_digest, response_len, response_summary";

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn events_for_trace(
    conn: &Connection,
    trace_id: &str,
) -> Result<Vec<TraceEventRow>, TraceStoreError> {
    events_for_trace_limited(conn, trace_id, usize::MAX)
}

pub(super) fn events_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceEventRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT seq, module, event, details_json FROM trace_events
         WHERE trace_id=?1 ORDER BY seq
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![trace_id, limit], |row| {
            Ok(TraceEventRow {
                seq: row.get(0)?,
                module: row.get(1)?,
                event: row.get(2)?,
                details_json: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn request_by_id(
    conn: &Connection,
    request_id: &str,
) -> Result<Option<TraceRequestRow>, TraceStoreError> {
    let sql = format!("SELECT {TRACE_REQUEST_COLUMNS} FROM trace_requests WHERE request_id=?1");
    conn.query_row(&sql, params![request_id], trace_request_row)
        .optional()
        .map_err(TraceStoreError::from)
}

pub(super) fn recent_requests(
    conn: &Connection,
    sandbox_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TraceRequestRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT {TRACE_REQUEST_COLUMNS}
         FROM trace_requests
         WHERE (?1 IS NULL OR sandbox_id=?1)
         ORDER BY sent_at_ms DESC, request_id DESC
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![sandbox_id, limit], trace_request_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn requests_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceRequestRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT {TRACE_REQUEST_COLUMNS}
         FROM trace_requests
         WHERE trace_id=?1
         ORDER BY sent_at_ms, request_id
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![trace_id, limit], trace_request_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn spans_for_trace(
    conn: &Connection,
    trace_id: &str,
) -> Result<Vec<TraceSpanRow>, TraceStoreError> {
    spans_for_trace_limited(conn, trace_id, usize::MAX)
}

pub(super) fn spans_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceSpanRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT trace_id, request_id, span_id, parent_span_id, kind, subsystem, status,
                started_us, duration_us, fields_json
         FROM trace_spans
         WHERE trace_id=?1
         ORDER BY started_us, span_id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![trace_id, limit], |row| {
            Ok(TraceSpanRow {
                trace_id: row.get(0)?,
                request_id: row.get(1)?,
                span_id: row.get(2)?,
                parent_span_id: row.get(3)?,
                kind: row.get(4)?,
                subsystem: row.get(5)?,
                status: row.get(6)?,
                started_us: row.get(7)?,
                duration_us: row.get(8)?,
                fields_json: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn resources_for_trace(
    conn: &Connection,
    trace_id: &str,
) -> Result<Vec<TraceResourceRow>, TraceStoreError> {
    resources_for_trace_limited(conn, trace_id, usize::MAX)
}

pub(super) fn resources_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceResourceRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT trace_id, request_id, span_id, ts_us, kind, values_json
         FROM trace_resources
         WHERE trace_id=?1
         ORDER BY ts_us, kind, span_id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![trace_id, limit], |row| {
            Ok(TraceResourceRow {
                trace_id: row.get(0)?,
                request_id: row.get(1)?,
                span_id: row.get(2)?,
                ts_us: row.get(3)?,
                kind: row.get(4)?,
                values_json: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn links_for_trace(
    conn: &Connection,
    trace_id: &str,
) -> Result<Vec<TraceLinkRow>, TraceStoreError> {
    links_for_trace_limited(conn, trace_id, usize::MAX)
}

pub(super) fn links_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceLinkRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT trace_id, link_kind, link_id, request_id
         FROM trace_links
         WHERE trace_id=?1
         ORDER BY link_kind, link_id, request_id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![trace_id, limit], |row| {
            Ok(TraceLinkRow {
                trace_id: row.get(0)?,
                link_kind: row.get(1)?,
                link_id: row.get(2)?,
                request_id: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn audit_entries_for_trace_limited(
    conn: &Connection,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceAuditEntryRow>, TraceStoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT audit_seq, sandbox_id, trace_id, request_id, entry_kind, schema_name,
                schema_version, received_at_ms, payload_sha256, prev_global_sha256,
                prev_sandbox_sha256, entry_sha256
         FROM audit_entries
         WHERE trace_id=?1
         ORDER BY audit_seq
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![trace_id, limit], |row| {
            Ok(TraceAuditEntryRow {
                audit_seq: row.get(0)?,
                sandbox_id: row.get(1)?,
                trace_id: row.get(2)?,
                request_id: row.get(3)?,
                entry_kind: row.get(4)?,
                schema_name: row.get(5)?,
                schema_version: row.get(6)?,
                received_at_ms: row.get(7)?,
                payload_sha256: row.get(8)?,
                prev_global_sha256: row.get(9)?,
                prev_sandbox_sha256: row.get(10)?,
                entry_sha256: row.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn audit_rows_for_verification(
    conn: &Connection,
) -> Result<Vec<AuditVerificationRow>, TraceStoreError> {
    let mut stmt = conn.prepare(
        "SELECT audit_seq, sandbox_id, trace_id, request_id, entry_kind, schema_name,
                schema_version, received_at_ms, payload, payload_sha256,
                prev_global_sha256, prev_sandbox_sha256, entry_sha256
         FROM audit_entries
         ORDER BY audit_seq",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AuditVerificationRow {
                audit_seq: row.get(0)?,
                sandbox_id: row.get(1)?,
                trace_id: row.get(2)?,
                request_id: row.get(3)?,
                entry_kind: row.get(4)?,
                schema_name: row.get(5)?,
                schema_version: row.get(6)?,
                received_at_ms: row.get(7)?,
                payload: row.get(8)?,
                payload_sha256: row.get(9)?,
                prev_global_sha256: row.get(10)?,
                prev_sandbox_sha256: row.get(11)?,
                entry_sha256: row.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn projection_gaps(
    conn: &Connection,
    trace_id: Option<&str>,
) -> Result<Vec<ProjectionGap>, TraceStoreError> {
    let mut stmt = conn.prepare(
        "SELECT a.audit_seq, a.request_id
         FROM audit_entries a
         LEFT JOIN trace_requests r ON r.request_id=a.request_id
         WHERE a.request_id IS NOT NULL
           AND a.entry_kind IN ('request_start', 'trace_degraded', 'response_persisted')
           AND r.request_id IS NULL
           AND (?1 IS NULL OR a.trace_id=?1)
         ORDER BY a.audit_seq",
    )?;
    let rows = stmt
        .query_map(params![trace_id], |row| {
            Ok(ProjectionGap {
                audit_seq: row.get(0)?,
                request_id: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn trace_ids_for_link(
    conn: &Connection,
    link_kind: &str,
    link_id: &str,
) -> Result<Vec<String>, TraceStoreError> {
    let mut stmt = conn.prepare(
        "SELECT trace_id FROM trace_links WHERE link_kind=?1 AND link_id=?2 ORDER BY trace_id",
    )?;
    let rows = stmt
        .query_map(params![link_kind, link_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn query_plan_for(conn: &Connection, sql: &str) -> Result<Vec<String>, TraceStoreError> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(3))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn resource_span_ids_for_request(
    conn: &Connection,
    request_id: &str,
) -> Result<Vec<Option<i64>>, TraceStoreError> {
    let mut stmt = conn.prepare(
        "SELECT span_id FROM trace_resources WHERE request_id=?1 ORDER BY kind, span_id, ts_us",
    )?;
    let rows = stmt
        .query_map(params![request_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(any(test, feature = "e2e-support"))]
pub(super) fn sqlite_posture(conn: &Connection) -> Result<SqlitePosture, TraceStoreError> {
    let journal_mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    Ok(SqlitePosture {
        journal_mode,
        synchronous,
    })
}

fn trace_request_row(row: &rusqlite::Row<'_>) -> Result<TraceRequestRow, rusqlite::Error> {
    Ok(TraceRequestRow {
        request_id: row.get(0)?,
        trace_id: row.get(1)?,
        sandbox_id: row.get(2)?,
        op: row.get(3)?,
        caller_id: row.get(4)?,
        args_summary: row.get(5)?,
        args_digest: row.get(6)?,
        workspace_route: row.get(7)?,
        status: row.get(8)?,
        error_kind: row.get(9)?,
        sent_at_ms: row.get(10)?,
        received_at_ms: row.get(11)?,
        host_rtt_ms: row.get(12)?,
        duration_us: row.get(13)?,
        daemon_boot_id: row.get(14)?,
        host_boot_id: row.get(15)?,
        modules_touched: row.get(16)?,
        response_digest: row.get(17)?,
        response_len: row.get(18)?,
        response_summary: row.get(19)?,
    })
}
