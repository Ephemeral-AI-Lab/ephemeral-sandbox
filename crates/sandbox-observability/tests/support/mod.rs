use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use sandbox_observability::{
    NamespaceExecutionSnapshotRecord, ObservabilityPaths, ResourceSampleRecord,
    WorkspaceSnapshotRecord,
};

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(name: &str) -> TestResult<Self> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sandbox-observability-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn test_paths(name: &str) -> TestResult<(TestDir, ObservabilityPaths)> {
    let dir = TestDir::new(name)?;
    let socket_path = dir.path().join("daemon-runtime").join("runtime.sock");
    let paths = ObservabilityPaths::from_socket_path(socket_path)?;
    Ok((dir, paths))
}

pub fn current_unix_ms() -> TestResult<i64> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    Ok(i64::try_from(millis).unwrap_or(i64::MAX))
}

pub fn allowed_tables() -> BTreeSet<String> {
    [
        "namespace_execution_snapshots",
        "resource_samples",
        "schema_migrations",
        "sandbox_snapshots",
        "workspace_snapshots",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn allowed_indexes() -> BTreeSet<String> {
    [
        "idx_namespace_execution_snapshots_workspace_session",
        "idx_resource_samples_sandbox_time",
        "idx_resource_samples_workspace_time",
        "idx_workspace_snapshots_sandbox",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub fn table_names(connection: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = connection.prepare(
        "SELECT name
             FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
    )?;

    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<_, _>>()
}

pub fn index_names(connection: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = connection.prepare(
        "SELECT name
             FROM sqlite_schema
             WHERE type = 'index'
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
    )?;

    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<_, _>>()
}

pub fn column_names(connection: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<Result<_, _>>()
}

pub fn migration_count(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get(0)
    })
}

pub fn row_count(connection: &Connection, table: &str) -> rusqlite::Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection.query_row(&sql, [], |row| row.get(0))
}

pub struct ResourceSampleRow {
    pub workspace_id: Option<String>,
    pub cgroup_available: bool,
    pub cgroup_error: Option<String>,
}

pub fn resource_sample_rows(
    connection: &Connection,
    sandbox_id: &str,
) -> rusqlite::Result<Vec<ResourceSampleRow>> {
    let mut statement = connection.prepare(
        "SELECT workspace_id, cgroup_available, cgroup_error
         FROM resource_samples
         WHERE sandbox_id = ?1
         ORDER BY sampled_at_unix_ms, sample_id",
    )?;
    let rows = statement.query_map([sandbox_id], |row| {
        Ok(ResourceSampleRow {
            workspace_id: row.get(0)?,
            cgroup_available: row.get::<_, i64>(1)? != 0,
            cgroup_error: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn workspace_snapshot(workspace_id: &str, sampled_at_unix_ms: i64) -> WorkspaceSnapshotRecord {
    WorkspaceSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        workspace_id: workspace_id.to_owned(),
        state: "active".to_owned(),
        profile: Some("host_compatible".to_owned()),
        workspace_root: Some(format!("/workspace/{workspace_id}")),
        upperdir: Some(format!("/workspace/{workspace_id}/upper")),
        workdir: Some(format!("/workspace/{workspace_id}/work")),
        namespace_fd_count: Some(3),
        base_manifest_version: Some(7),
        base_root_hash: Some("root-hash".to_owned()),
        layer_count: Some(2),
        sampled_at_unix_ms,
        error_message: None,
    }
}

pub fn namespace_execution_snapshot(
    namespace_execution_id: &str,
    workspace_session_id: &str,
    sampled_at_unix_ms: i64,
) -> NamespaceExecutionSnapshotRecord {
    NamespaceExecutionSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        namespace_execution_id: namespace_execution_id.to_owned(),
        workspace_session_id: workspace_session_id.to_owned(),
        operation: "exec_command".to_owned(),
        lifecycle_state: "running".to_owned(),
        sampled_at_unix_ms,
        error_message: None,
    }
}

pub fn resource_sample(
    sample_id: &str,
    workspace_id: Option<&str>,
    sampled_at_unix_ms: i64,
) -> ResourceSampleRecord {
    ResourceSampleRecord {
        sample_id: sample_id.to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        workspace_id: workspace_id.map(str::to_owned),
        sampled_at_unix_ms,
        cgroup_path: None,
        cgroup_available: false,
        cgroup_error: Some("cgroup path unavailable".to_owned()),
        cpu_usage_usec: None,
        cpu_usage_delta_usec: None,
        sample_delta_ms: None,
        memory_current_bytes: None,
        memory_current_delta_bytes: None,
        memory_max_bytes: None,
        memory_max_unlimited: None,
        disk_upperdir_bytes: Some(4096),
        disk_upperdir_delta_bytes: None,
        disk_file_count: Some(1),
        disk_dir_count: Some(1),
        disk_symlink_count: Some(0),
        disk_truncated: Some(false),
        disk_read_error_count: Some(0),
        disk_first_error_path: None,
    }
}
