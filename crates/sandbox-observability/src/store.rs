mod read;
mod rows;
mod schema;

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use thiserror::Error;

use crate::paths::ObservabilityPaths;
use crate::records::{
    NamespaceExecutionSnapshotRecord, RecordValidationError, ResourceSampleRecord,
    SandboxSnapshotRecord, WorkspaceSnapshotRecord,
};

pub use rows::{
    ObservabilityNamespaceExecutionSnapshotRow, ObservabilityResourceSampleRow,
    ObservabilitySandboxSnapshotRow, ObservabilitySnapshotReadOptions, ObservabilitySnapshotRows,
    ObservabilityWorkspaceSnapshotRow,
};

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
        schema::apply_schema(&mut connection)?;

        Ok(Self {
            connection: Mutex::new(connection),
        })
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
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                ON CONFLICT(sandbox_id, workspace_id) DO UPDATE SET
                    state = excluded.state,
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

    pub fn replace_namespace_execution_snapshots(
        &self,
        sandbox_id: &str,
        snapshots: &[NamespaceExecutionSnapshotRecord],
    ) -> Result<(), StoreError> {
        validate_id("sandbox_id", sandbox_id)?;
        for snapshot in snapshots {
            snapshot.validate_for_sandbox(sandbox_id)?;
        }
        let active = snapshots
            .iter()
            .map(|snapshot| snapshot.namespace_execution_id.as_str())
            .collect::<HashSet<_>>();

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

        let stale_namespace_execution_ids = {
            let mut statement = transaction.prepare(
                "SELECT namespace_execution_id
                    FROM namespace_execution_snapshots
                    WHERE sandbox_id = ?1",
            )?;
            let rows = statement.query_map([sandbox_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|namespace_execution_id| !active.contains(namespace_execution_id.as_str()))
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
                    cpu_usage_delta_usec,
                    sample_delta_ms,
                    memory_current_bytes,
                    memory_current_delta_bytes,
                    memory_max_bytes,
                    memory_max_unlimited,
                    disk_upperdir_bytes,
                    disk_upperdir_delta_bytes,
                    disk_file_count,
                    disk_dir_count,
                    disk_symlink_count,
                    disk_truncated,
                    disk_read_error_count,
                    disk_first_error_path
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    &sample.sample_id,
                    &sample.sandbox_id,
                    &sample.workspace_id,
                    sample.sampled_at_unix_ms,
                    &sample.cgroup_path,
                    bool_to_i64(sample.cgroup_available),
                    &sample.cgroup_error,
                    sample.cpu_usage_usec,
                    sample.cpu_usage_delta_usec,
                    sample.sample_delta_ms,
                    sample.memory_current_bytes,
                    sample.memory_current_delta_bytes,
                    sample.memory_max_bytes,
                    sample.memory_max_unlimited.map(bool_to_i64),
                    sample.disk_upperdir_bytes,
                    sample.disk_upperdir_delta_bytes,
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

    pub fn read_observability_snapshot(
        &self,
        sandbox_id: &str,
        options: &ObservabilitySnapshotReadOptions,
    ) -> Result<ObservabilitySnapshotRows, StoreError> {
        validate_id("sandbox_id", sandbox_id)?;

        let connection = self.connection()?;
        let sandbox = read::read_sandbox_snapshot(&connection, sandbox_id)?;
        let workspaces = read::read_active_workspace_snapshots(&connection, sandbox_id)?;
        let active_namespace_executions =
            read::read_active_namespace_execution_snapshots(&connection, sandbox_id)?;
        let latest_resources =
            read::read_latest_resource_samples(&connection, sandbox_id, &workspaces)?;
        let resource_history = match options.resource_window_ms {
            Some(window_ms) => read::read_resource_history(&connection, sandbox_id, window_ms)?,
            None => Vec::new(),
        };
        Ok(ObservabilitySnapshotRows {
            sandbox,
            workspaces,
            active_namespace_executions,
            latest_resources,
            resource_history,
        })
    }

    pub(super) fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
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

fn unix_time_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
