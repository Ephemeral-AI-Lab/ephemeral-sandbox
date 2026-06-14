use std::time::Duration;

use rusqlite::Connection;

use super::TraceStoreError;

pub(super) const STORE_SCHEMA_VERSION: u32 = 3;

pub(super) fn initialize(conn: &Connection) -> Result<(), TraceStoreError> {
    apply_pragmas(conn)?;
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > STORE_SCHEMA_VERSION {
        return Err(TraceStoreError::NewerSchema {
            found: version,
            supported: STORE_SCHEMA_VERSION,
        });
    }
    apply_migrations(conn, version)?;
    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
    Ok(())
}

fn apply_pragmas(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn apply_migrations(conn: &Connection, version: u32) -> Result<(), rusqlite::Error> {
    if version < 2 {
        migrate_to_v2(conn)?;
    }
    if version < 3 {
        migrate_to_v3(conn)?;
    }
    Ok(())
}

fn migrate_to_v2(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS trace_spool_drop_cursors (
          sandbox_id            TEXT NOT NULL,
          daemon_boot_id        TEXT NOT NULL,
          dropped_traces_total  INTEGER NOT NULL,
          updated_at_ms         INTEGER NOT NULL,
          PRIMARY KEY (sandbox_id, daemon_boot_id)
        );
        "#,
    )?;
    ensure_column(
        conn,
        "trace_spool_drop_cursors",
        "daemon_boot_id",
        "TEXT NOT NULL DEFAULT '_unknown'",
    )?;
    ensure_column(
        conn,
        "trace_spool_drop_cursors",
        "dropped_traces_total",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "trace_spool_drop_cursors",
        "updated_at_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

fn migrate_to_v3(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS trace_export_batches (
          export_id       TEXT PRIMARY KEY,
          sandbox_id      TEXT NOT NULL,
          daemon_boot_id  TEXT,
          batch_sha256    TEXT NOT NULL,
          record_count    INTEGER NOT NULL,
          ingested_at_ms  INTEGER NOT NULL,
          acked_at_ms     INTEGER,
          retry_count     INTEGER NOT NULL DEFAULT 0,
          last_ack_error  TEXT
        );
        "#,
    )?;
    ensure_column(
        conn,
        "trace_export_batches",
        "sandbox_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(conn, "trace_export_batches", "daemon_boot_id", "TEXT")?;
    ensure_column(
        conn,
        "trace_export_batches",
        "batch_sha256",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "trace_export_batches",
        "record_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "trace_export_batches",
        "ingested_at_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "trace_export_batches", "acked_at_ms", "INTEGER")?;
    ensure_column(
        conn,
        "trace_export_batches",
        "retry_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "trace_export_batches", "last_ack_error", "TEXT")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_trace_export_batches_sandbox
         ON trace_export_batches(sandbox_id, ingested_at_ms);",
    )
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), rusqlite::Error> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS audit_entries (
  audit_seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  sandbox_id            TEXT NOT NULL,
  trace_id              TEXT NOT NULL,
  request_id            TEXT,
  entry_kind            TEXT NOT NULL,
  schema_name           TEXT NOT NULL,
  schema_version        INTEGER NOT NULL,
  received_at_ms        INTEGER NOT NULL,
  payload               BLOB NOT NULL,
  payload_sha256        TEXT NOT NULL,
  prev_global_sha256    TEXT,
  prev_sandbox_sha256   TEXT,
  entry_sha256          TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS trace_requests (
  request_id       TEXT PRIMARY KEY,
  trace_id         TEXT NOT NULL,
  sandbox_id       TEXT NOT NULL,
  op               TEXT NOT NULL,
  family           TEXT NOT NULL,
  caller_id        TEXT,
  args_summary     TEXT,
  args_digest      TEXT,
  workspace_route  TEXT CHECK (workspace_route IN
    ('ephemeral_workspace','isolated_workspace','fast_path','none') OR workspace_route IS NULL),
  status           TEXT,
  error_kind       TEXT,
  sent_at_ms       INTEGER NOT NULL,
  received_at_ms   INTEGER,
  host_rtt_ms      INTEGER,
  duration_us      INTEGER,
  daemon_boot_id   TEXT,
  host_boot_id     TEXT NOT NULL,
  modules_touched  TEXT,
  response_digest  TEXT,
  response_len     INTEGER,
  response_summary TEXT
);
CREATE TABLE IF NOT EXISTS trace_spans (
  trace_id        TEXT NOT NULL,
  request_id      TEXT,
  span_id         INTEGER NOT NULL,
  parent_span_id  INTEGER,
  kind            TEXT NOT NULL,
  subsystem       TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'ok',
  started_us      INTEGER NOT NULL,
  duration_us     INTEGER NOT NULL,
  fields_json     TEXT,
  PRIMARY KEY (trace_id, span_id)
);
CREATE TABLE IF NOT EXISTS trace_events (
  trace_id    TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  request_id  TEXT,
  span_id     INTEGER,
  module      TEXT NOT NULL,
  event       TEXT NOT NULL,
  level       TEXT NOT NULL DEFAULT 'info',
  ts_us       INTEGER NOT NULL,
  details_json TEXT,
  PRIMARY KEY (trace_id, seq)
);
CREATE TABLE IF NOT EXISTS trace_resources (
  trace_id TEXT NOT NULL, request_id TEXT, span_id INTEGER,
  ts_us INTEGER NOT NULL, kind TEXT NOT NULL, values_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trace_links (
  trace_id  TEXT NOT NULL,
  link_kind TEXT NOT NULL,
  link_id   TEXT NOT NULL,
  request_id TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (trace_id, link_kind, link_id, request_id)
);
CREATE TABLE IF NOT EXISTS sandbox_heartbeats (
  sandbox_id        TEXT NOT NULL,
  ts_ms             INTEGER NOT NULL,
  daemon_boot_id    TEXT,
  reachable         INTEGER NOT NULL,
  uptime_s          REAL,
  manifest_version  INTEGER, manifest_depth INTEGER,
  active_leases     INTEGER, storage_bytes INTEGER, layer_dirs INTEGER, staging_dirs INTEGER,
  open_isolated     INTEGER, overlay_mounts INTEGER,
  active_commands   INTEGER, running_commands INTEGER, completed_unclaimed_commands INTEGER,
  plugin_services_ok INTEGER, plugin_services_failed INTEGER,
  cpu_usage_usec    INTEGER, cpu_throttled_usec INTEGER, cpu_nr_throttled INTEGER,
  memory_current_bytes INTEGER, memory_peak_bytes INTEGER,
  memory_oom_events INTEGER, memory_oom_kill_events INTEGER,
  io_rbytes         INTEGER, io_wbytes INTEGER, process_rss_bytes INTEGER,
  inflight_requests INTEGER, spool_pending INTEGER, spool_dropped_total INTEGER,
  details_json      TEXT,
  PRIMARY KEY (sandbox_id, ts_ms)
);
CREATE TABLE IF NOT EXISTS pending_trace_sidecars (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  sandbox_id      TEXT NOT NULL,
  trace_id        TEXT NOT NULL,
  request_id      TEXT NOT NULL,
  batch           BLOB NOT NULL,
  first_error     TEXT NOT NULL,
  last_error      TEXT NOT NULL,
  retry_count     INTEGER NOT NULL DEFAULT 0,
  created_at_ms   INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS trace_spool_drop_cursors (
  sandbox_id            TEXT NOT NULL,
  daemon_boot_id        TEXT NOT NULL,
  dropped_traces_total  INTEGER NOT NULL,
  updated_at_ms         INTEGER NOT NULL,
  PRIMARY KEY (sandbox_id, daemon_boot_id)
);
CREATE TABLE IF NOT EXISTS trace_export_batches (
  export_id       TEXT PRIMARY KEY,
  sandbox_id      TEXT NOT NULL,
  daemon_boot_id  TEXT,
  batch_sha256    TEXT NOT NULL,
  record_count    INTEGER NOT NULL,
  ingested_at_ms  INTEGER NOT NULL,
  acked_at_ms     INTEGER,
  retry_count     INTEGER NOT NULL DEFAULT 0,
  last_ack_error  TEXT
);
CREATE INDEX IF NOT EXISTS idx_hb_time         ON sandbox_heartbeats(ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_trace     ON audit_entries(trace_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_sandbox   ON audit_entries(sandbox_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_request   ON audit_entries(request_id);
CREATE INDEX IF NOT EXISTS idx_requests_trace  ON trace_requests(trace_id);
CREATE INDEX IF NOT EXISTS idx_requests_sent   ON trace_requests(sent_at_ms);
CREATE INDEX IF NOT EXISTS idx_requests_status ON trace_requests(status);
CREATE INDEX IF NOT EXISTS idx_spans_kind      ON trace_spans(kind);
CREATE INDEX IF NOT EXISTS idx_spans_request   ON trace_spans(request_id);
CREATE INDEX IF NOT EXISTS idx_events_request  ON trace_events(request_id);
CREATE INDEX IF NOT EXISTS idx_resources_trace ON trace_resources(trace_id, ts_us);
CREATE INDEX IF NOT EXISTS idx_resources_request_span_kind ON trace_resources(request_id, span_id, kind);
CREATE INDEX IF NOT EXISTS idx_links_id       ON trace_links(link_kind, link_id);
CREATE INDEX IF NOT EXISTS idx_events_span    ON trace_events(trace_id, span_id);
CREATE INDEX IF NOT EXISTS idx_events_event   ON trace_events(event);
CREATE INDEX IF NOT EXISTS idx_pending_sidecar_request ON pending_trace_sidecars(request_id);
CREATE INDEX IF NOT EXISTS idx_trace_export_batches_sandbox ON trace_export_batches(sandbox_id, ingested_at_ms);
"#;
