use rusqlite::{params, Connection, OptionalExtension};

use super::rows::{
    ObservabilityNamespaceExecutionSnapshotRow, ObservabilityResourceSampleRow,
    ObservabilitySandboxSnapshotRow, ObservabilityWorkspaceSnapshotRow,
};
use super::{unix_time_ms, StoreError};

pub(super) fn read_sandbox_snapshot(
    connection: &Connection,
    sandbox_id: &str,
) -> Result<Option<ObservabilitySandboxSnapshotRow>, StoreError> {
    connection
        .query_row(
            "SELECT
                sandbox_id,
                state,
                daemon_runtime_dir,
                socket_path,
                pid_path,
                daemon_pid,
                sampled_at_unix_ms,
                error_message
             FROM sandbox_snapshots
             WHERE sandbox_id = ?1",
            [sandbox_id],
            sandbox_snapshot_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(super) fn read_active_workspace_snapshots(
    connection: &Connection,
    sandbox_id: &str,
) -> Result<Vec<ObservabilityWorkspaceSnapshotRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT
            workspace_id,
            state,
            profile,
            namespace_fd_count,
            base_manifest_version,
            base_root_hash,
            layer_count,
            sampled_at_unix_ms,
            error_message
         FROM workspace_snapshots
         WHERE sandbox_id = ?1
           AND state != 'destroyed'
         ORDER BY workspace_id",
    )?;
    let rows = statement.query_map([sandbox_id], workspace_snapshot_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub(super) fn read_active_namespace_execution_snapshots(
    connection: &Connection,
    sandbox_id: &str,
) -> Result<Vec<ObservabilityNamespaceExecutionSnapshotRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT
            namespace_execution_id,
            workspace_session_id,
            operation,
            lifecycle_state,
            sampled_at_unix_ms,
            error_message
         FROM namespace_execution_snapshots
         WHERE sandbox_id = ?1
         ORDER BY workspace_session_id, namespace_execution_id",
    )?;
    let rows = statement.query_map([sandbox_id], namespace_execution_snapshot_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

pub(super) fn read_latest_resource_samples(
    connection: &Connection,
    sandbox_id: &str,
    workspaces: &[ObservabilityWorkspaceSnapshotRow],
) -> Result<Vec<ObservabilityResourceSampleRow>, StoreError> {
    let mut latest = Vec::new();
    if let Some(sample) = read_latest_resource_sample(connection, sandbox_id, None)? {
        latest.push(sample);
    }
    for workspace in workspaces {
        if let Some(sample) =
            read_latest_resource_sample(connection, sandbox_id, Some(&workspace.workspace_id))?
        {
            latest.push(sample);
        }
    }
    Ok(latest)
}

pub(super) fn read_resource_history(
    connection: &Connection,
    sandbox_id: &str,
    window_ms: u64,
) -> Result<Vec<ObservabilityResourceSampleRow>, StoreError> {
    let cutoff = unix_time_ms().saturating_sub(i64::try_from(window_ms).unwrap_or(i64::MAX));
    let mut statement = connection.prepare(
        &(RESOURCE_SAMPLE_SUMMARY_SELECT_PREFIX.to_owned()
            + " WHERE sandbox_id = ?1
                  AND sampled_at_unix_ms >= ?2
                ORDER BY sampled_at_unix_ms DESC, workspace_id, sample_id"),
    )?;
    let rows = statement.query_map(
        params![sandbox_id, cutoff],
        resource_sample_summary_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn read_latest_resource_sample(
    connection: &Connection,
    sandbox_id: &str,
    workspace_id: Option<&str>,
) -> Result<Option<ObservabilityResourceSampleRow>, StoreError> {
    match workspace_id {
        Some(workspace_id) => connection
            .query_row(
                &(RESOURCE_SAMPLE_SUMMARY_SELECT_PREFIX.to_owned()
                    + " WHERE sandbox_id = ?1
                          AND workspace_id = ?2
                        ORDER BY sampled_at_unix_ms DESC, sample_id DESC
                        LIMIT 1"),
                params![sandbox_id, workspace_id],
                resource_sample_summary_from_row,
            )
            .optional()
            .map_err(StoreError::from),
        None => connection
            .query_row(
                &(RESOURCE_SAMPLE_SUMMARY_SELECT_PREFIX.to_owned()
                    + " WHERE sandbox_id = ?1
                          AND workspace_id IS NULL
                        ORDER BY sampled_at_unix_ms DESC, sample_id DESC
                        LIMIT 1"),
                [sandbox_id],
                resource_sample_summary_from_row,
            )
            .optional()
            .map_err(StoreError::from),
    }
}

const RESOURCE_SAMPLE_SUMMARY_SELECT_PREFIX: &str = "SELECT
    workspace_id,
    sampled_at_unix_ms,
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
 FROM resource_samples";

fn sandbox_snapshot_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ObservabilitySandboxSnapshotRow> {
    Ok(ObservabilitySandboxSnapshotRow {
        sandbox_id: row.get(0)?,
        state: row.get(1)?,
        daemon_runtime_dir: row.get(2)?,
        socket_path: row.get(3)?,
        pid_path: row.get(4)?,
        daemon_pid: row.get(5)?,
        sampled_at_unix_ms: row.get(6)?,
        error_message: row.get(7)?,
    })
}

fn workspace_snapshot_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ObservabilityWorkspaceSnapshotRow> {
    Ok(ObservabilityWorkspaceSnapshotRow {
        workspace_id: row.get(0)?,
        state: row.get(1)?,
        profile: row.get(2)?,
        namespace_fd_count: row.get(3)?,
        base_manifest_version: row.get(4)?,
        base_root_hash: row.get(5)?,
        layer_count: row.get(6)?,
        sampled_at_unix_ms: row.get(7)?,
        error_message: row.get(8)?,
    })
}

fn namespace_execution_snapshot_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ObservabilityNamespaceExecutionSnapshotRow> {
    Ok(ObservabilityNamespaceExecutionSnapshotRow {
        namespace_execution_id: row.get(0)?,
        workspace_session_id: row.get(1)?,
        operation: row.get(2)?,
        lifecycle_state: row.get(3)?,
        sampled_at_unix_ms: row.get(4)?,
        error_message: row.get(5)?,
    })
}

fn resource_sample_summary_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ObservabilityResourceSampleRow> {
    Ok(ObservabilityResourceSampleRow {
        workspace_id: row.get(0)?,
        sampled_at_unix_ms: row.get(1)?,
        cgroup_available: row.get::<_, i64>(2)? != 0,
        cgroup_error: row.get(3)?,
        cpu_usage_usec: row.get(4)?,
        cpu_usage_delta_usec: row.get(5)?,
        sample_delta_ms: row.get(6)?,
        memory_current_bytes: row.get(7)?,
        memory_current_delta_bytes: row.get(8)?,
        memory_max_bytes: row.get(9)?,
        memory_max_unlimited: row.get::<_, Option<i64>>(10)?.map(|value| value != 0),
        disk_upperdir_bytes: row.get(11)?,
        disk_upperdir_delta_bytes: row.get(12)?,
        disk_file_count: row.get(13)?,
        disk_dir_count: row.get(14)?,
        disk_symlink_count: row.get(15)?,
        disk_truncated: row.get::<_, Option<i64>>(16)?.map(|value| value != 0),
        disk_read_error_count: row.get(17)?,
        disk_first_error_path: row.get(18)?,
    })
}
