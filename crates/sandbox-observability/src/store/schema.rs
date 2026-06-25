use rusqlite::{params, Connection, OptionalExtension};

use super::{unix_time_ms, StoreError};

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
    Migration {
        version: 6,
        name: "phase_4_7_trace_namespace_execution_id_rename",
        sql: V6_SCHEMA_SQL,
    },
    Migration {
        version: 7,
        name: "phase_5_drop_trace_tables",
        sql: V7_SCHEMA_SQL,
    },
    Migration {
        version: 8,
        name: "phase_6_resource_sample_deltas",
        sql: V8_SCHEMA_SQL,
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

const V6_SCHEMA_SQL: &str = r#"
ALTER TABLE traces RENAME COLUMN command_session_id TO namespace_execution_id;
"#;

const V7_SCHEMA_SQL: &str = r#"
DROP INDEX IF EXISTS idx_spans_trace_call_index;
DROP INDEX IF EXISTS idx_traces_request;
DROP INDEX IF EXISTS idx_traces_sandbox_started;
DROP INDEX IF EXISTS idx_namespace_execution_traces_namespace_execution;
DROP INDEX IF EXISTS idx_namespace_execution_traces_workspace_session_started;
DROP TABLE IF EXISTS spans;
DROP TABLE IF EXISTS traces;
DROP TABLE IF EXISTS namespace_execution_traces;
"#;

const V8_SCHEMA_SQL: &str = r#"
ALTER TABLE resource_samples ADD COLUMN cpu_usage_delta_usec INTEGER;
ALTER TABLE resource_samples ADD COLUMN sample_delta_ms INTEGER;
ALTER TABLE resource_samples ADD COLUMN memory_current_delta_bytes INTEGER;
ALTER TABLE resource_samples ADD COLUMN disk_upperdir_delta_bytes INTEGER;
"#;

pub(super) fn apply_schema(connection: &mut Connection) -> Result<(), StoreError> {
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
