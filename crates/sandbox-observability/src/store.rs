use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::paths::ObservabilityPaths;
use crate::records::{
    NamespaceExecutionSnapshotRecord, NamespaceExecutionTraceRecord, RecordValidationError,
    ResourceSampleRecord, SandboxSnapshotRecord, SpanRecord, TraceRecord, WorkspaceSnapshotRecord,
};

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "phase_1_observability_foundation",
        sql: V1_SCHEMA_SQL,
    },
    Migration {
        version: 2,
        name: "phase_2_runtime_snapshots",
        sql: V2_SCHEMA_SQL,
    },
    Migration {
        version: 3,
        name: "phase_4_async_method_traces",
        sql: V3_SCHEMA_SQL,
    },
    Migration {
        version: 4,
        name: "phase_4_5_namespace_execution_traces",
        sql: V4_SCHEMA_SQL,
    },
    Migration {
        version: 5,
        name: "phase_4_6_mechanical_namespace_execution_unification",
        sql: V5_SCHEMA_SQL,
    },
];

const SCHEMA_MIGRATIONS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  checksum TEXT NOT NULL,
  applied_at_unix_ms INTEGER NOT NULL
);
"#;

const V1_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS traces (
  trace_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  sandbox_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  request_id TEXT,
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER,
  duration_ms REAL,
  error_kind TEXT,
  error_message TEXT
);

CREATE TABLE IF NOT EXISTS spans (
  span_id TEXT PRIMARY KEY,
  trace_id TEXT NOT NULL,
  parent_span_id TEXT,
  method_name TEXT NOT NULL,
  call_index INTEGER NOT NULL,
  status TEXT NOT NULL,
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER,
  duration_ms REAL,
  error_kind TEXT,
  error_message TEXT,
  FOREIGN KEY(trace_id) REFERENCES traces(trace_id) ON DELETE CASCADE,
  FOREIGN KEY(parent_span_id) REFERENCES spans(span_id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS sandbox_snapshots (
  sandbox_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_traces_request
  ON traces(request_id);

CREATE INDEX IF NOT EXISTS idx_traces_sandbox_started
  ON traces(sandbox_id, started_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_spans_trace_call_index
  ON spans(trace_id, call_index);
"#;

const V2_SCHEMA_SQL: &str = r#"
ALTER TABLE sandbox_snapshots ADD COLUMN workspace_root TEXT;
ALTER TABLE sandbox_snapshots ADD COLUMN daemon_runtime_dir TEXT;
ALTER TABLE sandbox_snapshots ADD COLUMN socket_path TEXT;
ALTER TABLE sandbox_snapshots ADD COLUMN pid_path TEXT;
ALTER TABLE sandbox_snapshots ADD COLUMN daemon_pid INTEGER;

CREATE TABLE IF NOT EXISTS workspace_snapshots (
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  state TEXT NOT NULL,
  remount_state TEXT,
  profile TEXT,
  workspace_root TEXT,
  upperdir TEXT,
  workdir TEXT,
  namespace_fd_count INTEGER,
  base_manifest_version INTEGER,
  base_root_hash TEXT,
  layer_count INTEGER,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT,
  PRIMARY KEY(sandbox_id, workspace_id)
);

CREATE TABLE IF NOT EXISTS execution_snapshots (
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  execution_id TEXT NOT NULL,
  execution_kind TEXT NOT NULL,
  operation TEXT,
  command_session_id TEXT,
  command TEXT,
  lifecycle_state TEXT NOT NULL,
  finalization_state TEXT NOT NULL,
  workspace_ownership TEXT,
  started_at_unix_ms INTEGER,
  wall_time_ms REAL,
  process_group_id INTEGER,
  transcript_path TEXT,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT,
  PRIMARY KEY(sandbox_id, execution_id)
);

CREATE TABLE IF NOT EXISTS resource_samples (
  sample_id TEXT PRIMARY KEY,
  sandbox_id TEXT NOT NULL,
  workspace_id TEXT,
  sampled_at_unix_ms INTEGER NOT NULL,

  cgroup_path TEXT,
  cgroup_available INTEGER NOT NULL,
  cgroup_error TEXT,

  cpu_usage_usec INTEGER,

  memory_current_bytes INTEGER,
  memory_max_bytes INTEGER,
  memory_max_unlimited INTEGER,

  disk_upperdir_bytes INTEGER,
  disk_file_count INTEGER,
  disk_dir_count INTEGER,
  disk_symlink_count INTEGER,
  disk_truncated INTEGER,
  disk_read_error_count INTEGER,
  disk_first_error_path TEXT
);

CREATE INDEX IF NOT EXISTS idx_workspace_snapshots_sandbox
  ON workspace_snapshots(sandbox_id, workspace_id);

CREATE INDEX IF NOT EXISTS idx_execution_snapshots_workspace
  ON execution_snapshots(sandbox_id, workspace_id);

CREATE INDEX IF NOT EXISTS idx_execution_snapshots_command
  ON execution_snapshots(sandbox_id, command_session_id);

CREATE INDEX IF NOT EXISTS idx_resource_samples_workspace_time
  ON resource_samples(sandbox_id, workspace_id, sampled_at_unix_ms);

CREATE INDEX IF NOT EXISTS idx_resource_samples_sandbox_time
  ON resource_samples(sandbox_id, sampled_at_unix_ms);
"#;

const V3_SCHEMA_SQL: &str = r#"
ALTER TABLE traces ADD COLUMN origin_request_id TEXT;
ALTER TABLE traces ADD COLUMN workspace_id TEXT;
ALTER TABLE traces ADD COLUMN command_session_id TEXT;
"#;

const V4_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS namespace_execution_snapshots (
  sandbox_id TEXT NOT NULL,
  namespace_execution_id TEXT NOT NULL,
  workspace_session_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  lifecycle_state TEXT NOT NULL,
  sampled_at_unix_ms INTEGER NOT NULL,
  error_message TEXT,
  PRIMARY KEY(sandbox_id, namespace_execution_id)
);

CREATE TABLE IF NOT EXISTS namespace_execution_traces (
  trace_id TEXT PRIMARY KEY,
  sandbox_id TEXT NOT NULL,
  namespace_execution_id TEXT NOT NULL,
  workspace_session_id TEXT NOT NULL,
  operation TEXT NOT NULL,
  request_id TEXT,
  status TEXT NOT NULL,
  exit_code INTEGER,
  started_at_unix_ms INTEGER NOT NULL,
  finished_at_unix_ms INTEGER NOT NULL,
  duration_ms REAL NOT NULL,
  error_kind TEXT,
  error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_namespace_execution_snapshots_workspace_session
  ON namespace_execution_snapshots(sandbox_id, workspace_session_id);

CREATE INDEX IF NOT EXISTS idx_namespace_execution_traces_namespace_execution
  ON namespace_execution_traces(sandbox_id, namespace_execution_id);

CREATE INDEX IF NOT EXISTS idx_namespace_execution_traces_workspace_session_started
  ON namespace_execution_traces(sandbox_id, workspace_session_id, started_at_unix_ms);
"#;

const V5_SCHEMA_SQL: &str = r#"
DROP INDEX IF EXISTS idx_execution_snapshots_workspace;
DROP INDEX IF EXISTS idx_execution_snapshots_command;
DROP TABLE IF EXISTS execution_snapshots;
"#;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("failed to create observability directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid record")]
    InvalidRecord(#[from] RecordValidationError),
    #[error("observability connection lock is poisoned")]
    ConnectionLock,
    #[error("schema migration {version} checksum mismatch: expected {expected}, found {actual}")]
    MigrationChecksumMismatch {
        version: i64,
        expected: String,
        actual: String,
    },
}

pub struct ObservabilityStore {
    connection: Mutex<Connection>,
}

impl ObservabilityStore {
    pub fn open(paths: &ObservabilityPaths) -> Result<Self, StoreError> {
        fs::create_dir_all(paths.observability_dir()).map_err(|source| {
            StoreError::CreateDirectory {
                path: paths.observability_dir().to_path_buf(),
                source,
            }
        })?;

        let mut connection = Connection::open(paths.database_path())?;
        configure_connection(&connection)?;
        apply_schema(&mut connection)?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn insert_trace(
        &self,
        trace: &TraceRecord,
        spans: &[SpanRecord],
    ) -> Result<(), StoreError> {
        trace.validate()?;
        for span in spans {
            span.validate_for_trace(&trace.trace_id)?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO traces (
                trace_id,
                kind,
                status,
                sandbox_id,
                operation,
                request_id,
                origin_request_id,
                workspace_id,
                command_session_id,
                started_at_unix_ms,
                finished_at_unix_ms,
                duration_ms,
                error_kind,
                error_message
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                &trace.trace_id,
                &trace.kind,
                &trace.status,
                &trace.sandbox_id,
                &trace.operation,
                &trace.request_id,
                &trace.origin_request_id,
                &trace.workspace_id,
                &trace.command_session_id,
                trace.started_at_unix_ms,
                trace.finished_at_unix_ms,
                trace.duration_ms,
                &trace.error_kind,
                &trace.error_message,
            ],
        )?;

        for span in spans {
            transaction.execute(
                "INSERT INTO spans (
                    span_id,
                    trace_id,
                    parent_span_id,
                    method_name,
                    call_index,
                    status,
                    started_at_unix_ms,
                    finished_at_unix_ms,
                    duration_ms,
                    error_kind,
                    error_message
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    &span.span_id,
                    &span.trace_id,
                    &span.parent_span_id,
                    &span.method_name,
                    span.call_index,
                    &span.status,
                    span.started_at_unix_ms,
                    span.finished_at_unix_ms,
                    span.duration_ms,
                    &span.error_kind,
                    &span.error_message,
                ],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_sandbox_snapshot(
        &self,
        snapshot: &SandboxSnapshotRecord,
    ) -> Result<(), StoreError> {
        snapshot.validate()?;

        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO sandbox_snapshots (
                sandbox_id,
                state,
                workspace_root,
                daemon_runtime_dir,
                socket_path,
                pid_path,
                daemon_pid,
                sampled_at_unix_ms,
                error_message
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(sandbox_id) DO UPDATE SET
                state = excluded.state,
                workspace_root = excluded.workspace_root,
                daemon_runtime_dir = excluded.daemon_runtime_dir,
                socket_path = excluded.socket_path,
                pid_path = excluded.pid_path,
                daemon_pid = excluded.daemon_pid,
                sampled_at_unix_ms = excluded.sampled_at_unix_ms,
                error_message = excluded.error_message",
            params![
                &snapshot.sandbox_id,
                &snapshot.state,
                &snapshot.workspace_root,
                &snapshot.daemon_runtime_dir,
                &snapshot.socket_path,
                &snapshot.pid_path,
                snapshot.daemon_pid,
                snapshot.sampled_at_unix_ms,
                &snapshot.error_message,
            ],
        )?;

        Ok(())
    }

    pub fn upsert_workspace_snapshots(
        &self,
        sandbox_id: &str,
        snapshots: &[WorkspaceSnapshotRecord],
    ) -> Result<(), StoreError> {
        for snapshot in snapshots {
            snapshot.validate_for_sandbox(sandbox_id)?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for snapshot in snapshots {
            transaction.execute(
                "INSERT INTO workspace_snapshots (
                    sandbox_id,
                    workspace_id,
                    state,
                    remount_state,
                    profile,
                    workspace_root,
                    upperdir,
                    workdir,
                    namespace_fd_count,
                    base_manifest_version,
                    base_root_hash,
                    layer_count,
                    sampled_at_unix_ms,
                    error_message
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(sandbox_id, workspace_id) DO UPDATE SET
                    state = excluded.state,
                    remount_state = excluded.remount_state,
                    profile = excluded.profile,
                    workspace_root = excluded.workspace_root,
                    upperdir = excluded.upperdir,
                    workdir = excluded.workdir,
                    namespace_fd_count = excluded.namespace_fd_count,
                    base_manifest_version = excluded.base_manifest_version,
                    base_root_hash = excluded.base_root_hash,
                    layer_count = excluded.layer_count,
                    sampled_at_unix_ms = excluded.sampled_at_unix_ms,
                    error_message = excluded.error_message",
                params![
                    &snapshot.sandbox_id,
                    &snapshot.workspace_id,
                    &snapshot.state,
                    &snapshot.remount_state,
                    &snapshot.profile,
                    &snapshot.workspace_root,
                    &snapshot.upperdir,
                    &snapshot.workdir,
                    snapshot.namespace_fd_count,
                    snapshot.base_manifest_version,
                    &snapshot.base_root_hash,
                    snapshot.layer_count,
                    snapshot.sampled_at_unix_ms,
                    &snapshot.error_message,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reconcile_workspace_snapshots(
        &self,
        sandbox_id: &str,
        active_workspace_ids: &[String],
        sampled_at_unix_ms: i64,
    ) -> Result<(), StoreError> {
        validate_id("sandbox_id", sandbox_id)?;
        for workspace_id in active_workspace_ids {
            validate_id("workspace_id", workspace_id)?;
        }
        let active = active_workspace_ids.iter().collect::<HashSet<_>>();

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let stale_workspace_ids = {
            let mut statement = transaction.prepare(
                "SELECT workspace_id
                    FROM workspace_snapshots
                    WHERE sandbox_id = ?1
                      AND state != 'destroyed'",
            )?;
            let rows = statement.query_map([sandbox_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|workspace_id| !active.contains(workspace_id))
                .collect::<Vec<_>>()
        };

        for workspace_id in stale_workspace_ids {
            transaction.execute(
                "UPDATE workspace_snapshots
                    SET state = 'destroyed',
                        sampled_at_unix_ms = ?3
                    WHERE sandbox_id = ?1
                      AND workspace_id = ?2",
                params![sandbox_id, &workspace_id, sampled_at_unix_ms],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_namespace_execution_snapshots(
        &self,
        sandbox_id: &str,
        snapshots: &[NamespaceExecutionSnapshotRecord],
    ) -> Result<(), StoreError> {
        for snapshot in snapshots {
            snapshot.validate_for_sandbox(sandbox_id)?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for snapshot in snapshots {
            transaction.execute(
                "INSERT INTO namespace_execution_snapshots (
                    sandbox_id,
                    namespace_execution_id,
                    workspace_session_id,
                    operation,
                    lifecycle_state,
                    sampled_at_unix_ms,
                    error_message
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(sandbox_id, namespace_execution_id) DO UPDATE SET
                    workspace_session_id = excluded.workspace_session_id,
                    operation = excluded.operation,
                    lifecycle_state = excluded.lifecycle_state,
                    sampled_at_unix_ms = excluded.sampled_at_unix_ms,
                    error_message = excluded.error_message",
                params![
                    &snapshot.sandbox_id,
                    &snapshot.namespace_execution_id,
                    &snapshot.workspace_session_id,
                    &snapshot.operation,
                    &snapshot.lifecycle_state,
                    snapshot.sampled_at_unix_ms,
                    &snapshot.error_message,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn prune_namespace_execution_snapshots(
        &self,
        sandbox_id: &str,
        active_namespace_execution_ids: &[String],
    ) -> Result<(), StoreError> {
        validate_id("sandbox_id", sandbox_id)?;
        for namespace_execution_id in active_namespace_execution_ids {
            validate_id("namespace_execution_id", namespace_execution_id)?;
        }
        let active = active_namespace_execution_ids
            .iter()
            .collect::<HashSet<_>>();

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let stale_namespace_execution_ids = {
            let mut statement = transaction.prepare(
                "SELECT namespace_execution_id
                    FROM namespace_execution_snapshots
                    WHERE sandbox_id = ?1",
            )?;
            let rows = statement.query_map([sandbox_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|namespace_execution_id| !active.contains(namespace_execution_id))
                .collect::<Vec<_>>()
        };

        for namespace_execution_id in stale_namespace_execution_ids {
            transaction.execute(
                "DELETE FROM namespace_execution_snapshots
                    WHERE sandbox_id = ?1
                      AND namespace_execution_id = ?2",
                params![sandbox_id, &namespace_execution_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_namespace_execution_trace(
        &self,
        trace: &NamespaceExecutionTraceRecord,
    ) -> Result<(), StoreError> {
        trace.validate()?;

        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO namespace_execution_traces (
                trace_id,
                sandbox_id,
                namespace_execution_id,
                workspace_session_id,
                operation,
                request_id,
                status,
                exit_code,
                started_at_unix_ms,
                finished_at_unix_ms,
                duration_ms,
                error_kind,
                error_message
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(trace_id) DO UPDATE SET
                sandbox_id = excluded.sandbox_id,
                namespace_execution_id = excluded.namespace_execution_id,
                workspace_session_id = excluded.workspace_session_id,
                operation = excluded.operation,
                request_id = excluded.request_id,
                status = excluded.status,
                exit_code = excluded.exit_code,
                started_at_unix_ms = excluded.started_at_unix_ms,
                finished_at_unix_ms = excluded.finished_at_unix_ms,
                duration_ms = excluded.duration_ms,
                error_kind = excluded.error_kind,
                error_message = excluded.error_message",
            params![
                &trace.trace_id,
                &trace.sandbox_id,
                &trace.namespace_execution_id,
                &trace.workspace_session_id,
                &trace.operation,
                &trace.request_id,
                &trace.status,
                trace.exit_code,
                trace.started_at_unix_ms,
                trace.finished_at_unix_ms,
                trace.duration_ms,
                &trace.error_kind,
                &trace.error_message,
            ],
        )?;
        Ok(())
    }

    pub fn insert_resource_samples(
        &self,
        samples: &[ResourceSampleRecord],
    ) -> Result<(), StoreError> {
        for sample in samples {
            sample.validate()?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for sample in samples {
            transaction.execute(
                "INSERT INTO resource_samples (
                    sample_id,
                    sandbox_id,
                    workspace_id,
                    sampled_at_unix_ms,
                    cgroup_path,
                    cgroup_available,
                    cgroup_error,
                    cpu_usage_usec,
                    memory_current_bytes,
                    memory_max_bytes,
                    memory_max_unlimited,
                    disk_upperdir_bytes,
                    disk_file_count,
                    disk_dir_count,
                    disk_symlink_count,
                    disk_truncated,
                    disk_read_error_count,
                    disk_first_error_path
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    &sample.sample_id,
                    &sample.sandbox_id,
                    &sample.workspace_id,
                    sample.sampled_at_unix_ms,
                    &sample.cgroup_path,
                    bool_to_i64(sample.cgroup_available),
                    &sample.cgroup_error,
                    sample.cpu_usage_usec,
                    sample.memory_current_bytes,
                    sample.memory_max_bytes,
                    sample.memory_max_unlimited.map(bool_to_i64),
                    sample.disk_upperdir_bytes,
                    sample.disk_file_count,
                    sample.disk_dir_count,
                    sample.disk_symlink_count,
                    sample.disk_truncated.map(bool_to_i64),
                    sample.disk_read_error_count,
                    &sample.disk_first_error_path,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn force_sqlite_write_errors_for_test(&self) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.pragma_update(None, "query_only", "ON")?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn trace_for_test(&self, trace_id: &str) -> Result<Option<TraceRecord>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT
                    trace_id,
                    kind,
                    status,
                    sandbox_id,
                    operation,
                    request_id,
                    origin_request_id,
                    workspace_id,
                    command_session_id,
                    started_at_unix_ms,
                    finished_at_unix_ms,
                    duration_ms,
                    error_kind,
                    error_message
                 FROM traces
                 WHERE trace_id = ?1",
                [trace_id],
                |row| {
                    Ok(TraceRecord {
                        trace_id: row.get(0)?,
                        kind: row.get(1)?,
                        status: row.get(2)?,
                        sandbox_id: row.get(3)?,
                        operation: row.get(4)?,
                        request_id: row.get(5)?,
                        origin_request_id: row.get(6)?,
                        workspace_id: row.get(7)?,
                        command_session_id: row.get(8)?,
                        started_at_unix_ms: row.get(9)?,
                        finished_at_unix_ms: row.get(10)?,
                        duration_ms: row.get(11)?,
                        error_kind: row.get(12)?,
                        error_message: row.get(13)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn spans_for_test(&self, trace_id: &str) -> Result<Vec<SpanRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                span_id,
                trace_id,
                parent_span_id,
                method_name,
                call_index,
                status,
                started_at_unix_ms,
                finished_at_unix_ms,
                duration_ms,
                error_kind,
                error_message
             FROM spans
             WHERE trace_id = ?1
             ORDER BY call_index",
        )?;
        let rows = statement.query_map([trace_id], |row| {
            Ok(SpanRecord {
                span_id: row.get(0)?,
                trace_id: row.get(1)?,
                parent_span_id: row.get(2)?,
                method_name: row.get(3)?,
                call_index: row.get(4)?,
                status: row.get(5)?,
                started_at_unix_ms: row.get(6)?,
                finished_at_unix_ms: row.get(7)?,
                duration_ms: row.get(8)?,
                error_kind: row.get(9)?,
                error_message: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn sandbox_snapshot_for_test(
        &self,
        sandbox_id: &str,
    ) -> Result<Option<SandboxSnapshotRecord>, StoreError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT
                    sandbox_id,
                    state,
                    workspace_root,
                    daemon_runtime_dir,
                    socket_path,
                    pid_path,
                    daemon_pid,
                    sampled_at_unix_ms,
                    error_message
                FROM sandbox_snapshots
                WHERE sandbox_id = ?1",
                [sandbox_id],
                |row| {
                    Ok(SandboxSnapshotRecord {
                        sandbox_id: row.get(0)?,
                        state: row.get(1)?,
                        workspace_root: row.get(2)?,
                        daemon_runtime_dir: row.get(3)?,
                        socket_path: row.get(4)?,
                        pid_path: row.get(5)?,
                        daemon_pid: row.get(6)?,
                        sampled_at_unix_ms: row.get(7)?,
                        error_message: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn workspace_snapshots_for_test(
        &self,
        sandbox_id: &str,
    ) -> Result<Vec<WorkspaceSnapshotRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                sandbox_id,
                workspace_id,
                state,
                remount_state,
                profile,
                workspace_root,
                upperdir,
                workdir,
                namespace_fd_count,
                base_manifest_version,
                base_root_hash,
                layer_count,
                sampled_at_unix_ms,
                error_message
            FROM workspace_snapshots
            WHERE sandbox_id = ?1
            ORDER BY workspace_id",
        )?;
        let rows = statement.query_map([sandbox_id], |row| {
            Ok(WorkspaceSnapshotRecord {
                sandbox_id: row.get(0)?,
                workspace_id: row.get(1)?,
                state: row.get(2)?,
                remount_state: row.get(3)?,
                profile: row.get(4)?,
                workspace_root: row.get(5)?,
                upperdir: row.get(6)?,
                workdir: row.get(7)?,
                namespace_fd_count: row.get(8)?,
                base_manifest_version: row.get(9)?,
                base_root_hash: row.get(10)?,
                layer_count: row.get(11)?,
                sampled_at_unix_ms: row.get(12)?,
                error_message: row.get(13)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn namespace_execution_snapshots_for_test(
        &self,
        sandbox_id: &str,
    ) -> Result<Vec<NamespaceExecutionSnapshotRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                sandbox_id,
                namespace_execution_id,
                workspace_session_id,
                operation,
                lifecycle_state,
                sampled_at_unix_ms,
                error_message
            FROM namespace_execution_snapshots
            WHERE sandbox_id = ?1
            ORDER BY namespace_execution_id",
        )?;
        let rows = statement.query_map([sandbox_id], |row| {
            Ok(NamespaceExecutionSnapshotRecord {
                sandbox_id: row.get(0)?,
                namespace_execution_id: row.get(1)?,
                workspace_session_id: row.get(2)?,
                operation: row.get(3)?,
                lifecycle_state: row.get(4)?,
                sampled_at_unix_ms: row.get(5)?,
                error_message: row.get(6)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn namespace_execution_traces_for_test(
        &self,
        sandbox_id: &str,
    ) -> Result<Vec<NamespaceExecutionTraceRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                trace_id,
                sandbox_id,
                namespace_execution_id,
                workspace_session_id,
                operation,
                request_id,
                status,
                exit_code,
                started_at_unix_ms,
                finished_at_unix_ms,
                duration_ms,
                error_kind,
                error_message
            FROM namespace_execution_traces
            WHERE sandbox_id = ?1
            ORDER BY namespace_execution_id",
        )?;
        let rows = statement.query_map([sandbox_id], |row| {
            Ok(NamespaceExecutionTraceRecord {
                trace_id: row.get(0)?,
                sandbox_id: row.get(1)?,
                namespace_execution_id: row.get(2)?,
                workspace_session_id: row.get(3)?,
                operation: row.get(4)?,
                request_id: row.get(5)?,
                status: row.get(6)?,
                exit_code: row.get(7)?,
                started_at_unix_ms: row.get(8)?,
                finished_at_unix_ms: row.get(9)?,
                duration_ms: row.get(10)?,
                error_kind: row.get(11)?,
                error_message: row.get(12)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn resource_samples_for_test(
        &self,
        sandbox_id: &str,
    ) -> Result<Vec<ResourceSampleRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
                sample_id,
                sandbox_id,
                workspace_id,
                sampled_at_unix_ms,
                cgroup_path,
                cgroup_available,
                cgroup_error,
                cpu_usage_usec,
                memory_current_bytes,
                memory_max_bytes,
                memory_max_unlimited,
                disk_upperdir_bytes,
                disk_file_count,
                disk_dir_count,
                disk_symlink_count,
                disk_truncated,
                disk_read_error_count,
                disk_first_error_path
            FROM resource_samples
            WHERE sandbox_id = ?1
            ORDER BY sampled_at_unix_ms, sample_id",
        )?;
        let rows = statement.query_map([sandbox_id], |row| {
            Ok(ResourceSampleRecord {
                sample_id: row.get(0)?,
                sandbox_id: row.get(1)?,
                workspace_id: row.get(2)?,
                sampled_at_unix_ms: row.get(3)?,
                cgroup_path: row.get(4)?,
                cgroup_available: row.get::<_, i64>(5)? != 0,
                cgroup_error: row.get(6)?,
                cpu_usage_usec: row.get(7)?,
                memory_current_bytes: row.get(8)?,
                memory_max_bytes: row.get(9)?,
                memory_max_unlimited: row.get::<_, Option<i64>>(10)?.map(|value| value != 0),
                disk_upperdir_bytes: row.get(11)?,
                disk_file_count: row.get(12)?,
                disk_dir_count: row.get(13)?,
                disk_symlink_count: row.get(14)?,
                disk_truncated: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
                disk_read_error_count: row.get(16)?,
                disk_first_error_path: row.get(17)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::ConnectionLock)
    }
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.busy_timeout(Duration::from_millis(1000))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn validate_id(field: &'static str, value: &str) -> Result<(), StoreError> {
    if value.is_empty() {
        return Err(RecordValidationError::Empty { field }.into());
    }
    Ok(())
}

const fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn apply_schema(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA_MIGRATIONS_SQL)?;

    for migration in MIGRATIONS {
        let expected_checksum = schema_checksum(migration.sql);
        let applied_checksum = transaction
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        match applied_checksum {
            Some(checksum) if checksum == expected_checksum => {}
            Some(actual) => {
                return Err(StoreError::MigrationChecksumMismatch {
                    version: migration.version,
                    expected: expected_checksum,
                    actual,
                });
            }
            None => {
                transaction.execute_batch(migration.sql)?;
                transaction.execute(
                    "INSERT INTO schema_migrations (
                        version,
                        name,
                        checksum,
                        applied_at_unix_ms
                    ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        migration.version,
                        migration.name,
                        &expected_checksum,
                        unix_time_ms()
                    ],
                )?;
            }
        }
    }

    transaction.commit()?;
    Ok(())
}

fn schema_checksum(sql: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in sql.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    format!("fnv1a64:{hash:016x}")
}

fn unix_time_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{schema_checksum, V1_SCHEMA_SQL};

    #[test]
    fn schema_checksum_changes_with_sql_text() {
        let checksum = schema_checksum(V1_SCHEMA_SQL);

        assert!(checksum.starts_with("fnv1a64:"));
        assert_ne!(checksum, schema_checksum("SELECT 1;"));
    }
}
