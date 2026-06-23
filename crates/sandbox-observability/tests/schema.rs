use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use sandbox_observability::{
    ExecutionSnapshotRecord, NamespaceExecutionSnapshotRecord, NamespaceExecutionTraceRecord,
    ObservabilityPaths, ObservabilityStore, ResourceSampleRecord, SandboxSnapshotRecord,
    SpanRecord, StoreError, TraceRecord, WorkspaceSnapshotRecord,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> TestResult<Self> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sandbox-observability-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn schema_initialization_is_idempotent() -> TestResult {
    let (dir, paths) = test_paths("schema-idempotent")?;

    let first_store = ObservabilityStore::open(&paths)?;
    drop(first_store);
    let second_store = ObservabilityStore::open(&paths)?;
    drop(second_store);

    let connection = Connection::open(paths.database_path())?;
    assert_eq!(table_names(&connection)?, allowed_tables());
    assert_eq!(index_names(&connection)?, allowed_indexes());
    let trace_columns = column_names(&connection, "traces")?;
    assert!(trace_columns.contains("origin_request_id"));
    assert!(trace_columns.contains("workspace_id"));
    assert!(trace_columns.contains("command_session_id"));
    let namespace_snapshot_columns = column_names(&connection, "namespace_execution_snapshots")?;
    assert!(namespace_snapshot_columns.contains("namespace_execution_id"));
    assert!(namespace_snapshot_columns.contains("workspace_session_id"));
    let namespace_trace_columns = column_names(&connection, "namespace_execution_traces")?;
    assert!(namespace_trace_columns.contains("namespace_execution_id"));
    assert!(namespace_trace_columns.contains("workspace_session_id"));
    assert!(namespace_trace_columns.contains("exit_code"));
    assert_eq!(migration_count(&connection)?, 4);
    assert!(paths.database_path().exists());
    assert!(dir
        .path()
        .join("daemon-runtime")
        .join("observability")
        .exists());

    Ok(())
}

#[test]
fn schema_initialization_rejects_migration_checksum_drift() -> TestResult {
    let (_dir, paths) = test_paths("schema-checksum-drift")?;
    let store = ObservabilityStore::open(&paths)?;
    drop(store);

    let connection = Connection::open(paths.database_path())?;
    connection.execute(
        "UPDATE schema_migrations
             SET checksum = 'fnv1a64:0000000000000000'
             WHERE version = 1",
        [],
    )?;
    drop(connection);

    let error = match ObservabilityStore::open(&paths) {
        Ok(_) => return Err("schema initialization accepted a stale checksum".into()),
        Err(error) => error,
    };

    match error {
        StoreError::MigrationChecksumMismatch {
            version,
            expected,
            actual,
        } => {
            assert_eq!(version, 1);
            assert!(expected.starts_with("fnv1a64:"));
            assert_eq!(actual, "fnv1a64:0000000000000000");
        }
        other => return Err(format!("unexpected schema initialization error: {other}").into()),
    }

    Ok(())
}

#[test]
fn inserts_synthetic_trace_and_spans() -> TestResult {
    let (_dir, paths) = test_paths("trace-span-insert")?;
    let store = ObservabilityStore::open(&paths)?;

    store.insert_trace(
        &TraceRecord {
            trace_id: "trace-1".to_owned(),
            kind: "request".to_owned(),
            status: "ok".to_owned(),
            sandbox_id: "sandbox-1".to_owned(),
            operation: "exec_command".to_owned(),
            request_id: Some("request-1".to_owned()),
            origin_request_id: None,
            workspace_id: None,
            command_session_id: None,
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: Some(1_025),
            duration_ms: Some(25.0),
            error_kind: None,
            error_message: None,
        },
        &[
            SpanRecord {
                span_id: "span-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                parent_span_id: None,
                method_name: "dispatch_operation".to_owned(),
                call_index: 0,
                status: "ok".to_owned(),
                started_at_unix_ms: 1_000,
                finished_at_unix_ms: Some(1_005),
                duration_ms: Some(5.0),
                error_kind: None,
                error_message: None,
            },
            SpanRecord {
                span_id: "span-2".to_owned(),
                trace_id: "trace-1".to_owned(),
                parent_span_id: Some("span-1".to_owned()),
                method_name: "CommandOperationService::exec_command".to_owned(),
                call_index: 1,
                status: "ok".to_owned(),
                started_at_unix_ms: 1_005,
                finished_at_unix_ms: Some(1_025),
                duration_ms: Some(20.0),
                error_kind: None,
                error_message: None,
            },
        ],
    )?;

    let connection = Connection::open(paths.database_path())?;
    assert_eq!(row_count(&connection, "traces")?, 1);
    assert_eq!(row_count(&connection, "spans")?, 2);

    let request_id: String = connection.query_row(
        "SELECT request_id FROM traces WHERE trace_id = 'trace-1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(request_id, "request-1");

    let max_call_index: i64 =
        connection.query_row("SELECT MAX(call_index) FROM spans", [], |row| row.get(0))?;
    assert_eq!(max_call_index, 1);

    Ok(())
}

#[test]
fn schema_inserts_async_trace_fields_without_adding_async_indexes() -> TestResult {
    let (_dir, paths) = test_paths("async-trace-insert")?;
    let store = ObservabilityStore::open(&paths)?;

    store.insert_trace(
        &TraceRecord {
            trace_id: "async:command_finalization:command_session_id:cmd_1".to_owned(),
            kind: "async".to_owned(),
            status: "ok".to_owned(),
            sandbox_id: "sandbox-1".to_owned(),
            operation: "command_finalization".to_owned(),
            request_id: None,
            origin_request_id: Some("request-1".to_owned()),
            workspace_id: Some("workspace-1".to_owned()),
            command_session_id: Some("cmd_1".to_owned()),
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: Some(1_010),
            duration_ms: Some(10.0),
            error_kind: None,
            error_message: None,
        },
        &[SpanRecord {
            span_id: "async:command_finalization:command_session_id:cmd_1:span:0".to_owned(),
            trace_id: "async:command_finalization:command_session_id:cmd_1".to_owned(),
            parent_span_id: None,
            method_name: "complete_terminal_command_with_services".to_owned(),
            call_index: 0,
            status: "ok".to_owned(),
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: Some(1_010),
            duration_ms: Some(10.0),
            error_kind: None,
            error_message: None,
        }],
    )?;

    let connection = Connection::open(paths.database_path())?;
    assert_eq!(index_names(&connection)?, allowed_indexes());
    let row: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT request_id, origin_request_id, workspace_id, command_session_id
             FROM traces
             WHERE trace_id = 'async:command_finalization:command_session_id:cmd_1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(
        row,
        (
            None,
            Some("request-1".to_owned()),
            Some("workspace-1".to_owned()),
            Some("cmd_1".to_owned()),
        )
    );
    Ok(())
}

#[test]
fn upserts_synthetic_sandbox_snapshot() -> TestResult {
    let (_dir, paths) = test_paths("snapshot-upsert")?;
    let store = ObservabilityStore::open(&paths)?;

    store.upsert_sandbox_snapshot(&SandboxSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        state: "starting".to_owned(),
        workspace_root: None,
        daemon_runtime_dir: Some("/tmp/daemon".to_owned()),
        socket_path: Some("/tmp/daemon/runtime.sock".to_owned()),
        pid_path: Some("/tmp/daemon/runtime.pid".to_owned()),
        daemon_pid: Some(42),
        sampled_at_unix_ms: 1_000,
        error_message: Some("warming up".to_owned()),
    })?;
    store.upsert_sandbox_snapshot(&SandboxSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        state: "ready".to_owned(),
        workspace_root: Some("/workspace".to_owned()),
        daemon_runtime_dir: Some("/tmp/daemon".to_owned()),
        socket_path: Some("/tmp/daemon/runtime.sock".to_owned()),
        pid_path: Some("/tmp/daemon/runtime.pid".to_owned()),
        daemon_pid: Some(43),
        sampled_at_unix_ms: 2_000,
        error_message: None,
    })?;

    let connection = Connection::open(paths.database_path())?;
    assert_eq!(row_count(&connection, "sandbox_snapshots")?, 1);

    let snapshot: (String, Option<String>, i64, Option<String>) = connection.query_row(
        "SELECT state, workspace_root, sampled_at_unix_ms, error_message
             FROM sandbox_snapshots
             WHERE sandbox_id = 'sandbox-1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    assert_eq!(
        snapshot,
        (
            "ready".to_owned(),
            Some("/workspace".to_owned()),
            2_000,
            None
        )
    );

    Ok(())
}

#[test]
fn workspace_upsert_marks_stale_rows_destroyed_and_keeps_resource_history() -> TestResult {
    let (_dir, paths) = test_paths("workspace-reconcile")?;
    let store = ObservabilityStore::open(&paths)?;

    store.upsert_workspace_snapshots(
        "sandbox-1",
        &[
            workspace_snapshot("workspace-1", 1_000),
            workspace_snapshot("workspace-2", 1_000),
        ],
    )?;
    store.insert_resource_samples(&[resource_sample(
        "sample-workspace-1",
        Some("workspace-1"),
        1_100,
    )])?;
    store.reconcile_workspace_snapshots("sandbox-1", &["workspace-2".to_owned()], 2_000)?;

    let connection = Connection::open(paths.database_path())?;
    let mut statement = connection.prepare(
        "SELECT workspace_id, state, sampled_at_unix_ms
         FROM workspace_snapshots
         WHERE sandbox_id = 'sandbox-1'
         ORDER BY workspace_id",
    )?;
    let workspaces = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, i64)>>>()?;
    assert_eq!(workspaces.len(), 2);
    assert_eq!(
        workspaces[0],
        ("workspace-1".to_owned(), "destroyed".to_owned(), 2_000)
    );
    assert_eq!(
        workspaces[1],
        ("workspace-2".to_owned(), "active".to_owned(), 1_000)
    );

    let samples = resource_sample_rows(&connection, "sandbox-1")?;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].workspace_id.as_deref(), Some("workspace-1"));

    Ok(())
}

#[test]
fn active_execution_upsert_and_prune_tracks_current_rows() -> TestResult {
    let (_dir, paths) = test_paths("execution-prune")?;
    let store = ObservabilityStore::open(&paths)?;

    store.upsert_execution_snapshots(
        "sandbox-1",
        &[
            execution_snapshot("exec-1", "workspace-1", 1_000),
            execution_snapshot("exec-2", "workspace-1", 1_000),
        ],
    )?;
    store.prune_execution_snapshots("sandbox-1", &["exec-2".to_owned()])?;

    let connection = Connection::open(paths.database_path())?;
    let execution: (String, String) = connection.query_row(
        "SELECT execution_id, execution_kind
         FROM execution_snapshots
         WHERE sandbox_id = 'sandbox-1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(execution, ("exec-2".to_owned(), "command".to_owned()));

    Ok(())
}

#[test]
fn namespace_execution_snapshot_and_completed_trace_use_typed_tables() -> TestResult {
    let (_dir, paths) = test_paths("namespace-execution")?;
    let store = ObservabilityStore::open(&paths)?;

    store.upsert_namespace_execution_snapshots(
        "sandbox-1",
        &[
            namespace_execution_snapshot("namespace_execution_1", "workspace-1", 1_000),
            namespace_execution_snapshot("namespace_execution_2", "workspace-1", 1_000),
        ],
    )?;
    store
        .prune_namespace_execution_snapshots("sandbox-1", &["namespace_execution_2".to_owned()])?;
    store.insert_namespace_execution_trace(&NamespaceExecutionTraceRecord {
        trace_id: "namespace_execution:namespace_execution_1".to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        namespace_execution_id: "namespace_execution_1".to_owned(),
        workspace_session_id: "workspace-1".to_owned(),
        operation: "exec_command".to_owned(),
        request_id: Some("request-1".to_owned()),
        status: "ok".to_owned(),
        exit_code: Some(0),
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: 1_025,
        duration_ms: 25.0,
        error_kind: None,
        error_message: None,
    })?;

    let connection = Connection::open(paths.database_path())?;
    assert_eq!(row_count(&connection, "execution_snapshots")?, 0);
    let snapshot: (String, String, String, String) = connection.query_row(
        "SELECT namespace_execution_id, workspace_session_id, operation, lifecycle_state
         FROM namespace_execution_snapshots
         WHERE sandbox_id = 'sandbox-1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(
        snapshot,
        (
            "namespace_execution_2".to_owned(),
            "workspace-1".to_owned(),
            "exec_command".to_owned(),
            "running".to_owned(),
        )
    );
    let trace: (String, String, String, Option<i64>) = connection.query_row(
        "SELECT namespace_execution_id, workspace_session_id, status, exit_code
         FROM namespace_execution_traces
         WHERE sandbox_id = 'sandbox-1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(
        trace,
        (
            "namespace_execution_1".to_owned(),
            "workspace-1".to_owned(),
            "ok".to_owned(),
            Some(0),
        )
    );

    Ok(())
}

#[test]
fn resource_samples_preserve_sandbox_and_workspace_scope() -> TestResult {
    let (_dir, paths) = test_paths("resource-scope")?;
    let store = ObservabilityStore::open(&paths)?;

    store.insert_resource_samples(&[
        resource_sample("sample-global", None, 1_000),
        resource_sample("sample-workspace", Some("workspace-1"), 1_001),
    ])?;

    let connection = Connection::open(paths.database_path())?;
    let samples = resource_sample_rows(&connection, "sandbox-1")?;
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].workspace_id, None);
    assert!(!samples[0].cgroup_available);
    assert_eq!(
        samples[0].cgroup_error.as_deref(),
        Some("cgroup path unavailable")
    );
    assert_eq!(samples[1].workspace_id.as_deref(), Some("workspace-1"));

    Ok(())
}

fn test_paths(name: &str) -> TestResult<(TestDir, ObservabilityPaths)> {
    let dir = TestDir::new(name)?;
    let socket_path = dir.path().join("daemon-runtime").join("runtime.sock");
    let paths = ObservabilityPaths::from_socket_path(socket_path)?;
    Ok((dir, paths))
}

fn allowed_tables() -> BTreeSet<String> {
    [
        "execution_snapshots",
        "namespace_execution_snapshots",
        "namespace_execution_traces",
        "resource_samples",
        "schema_migrations",
        "sandbox_snapshots",
        "spans",
        "traces",
        "workspace_snapshots",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn allowed_indexes() -> BTreeSet<String> {
    [
        "idx_execution_snapshots_command",
        "idx_execution_snapshots_workspace",
        "idx_namespace_execution_snapshots_workspace_session",
        "idx_namespace_execution_traces_namespace_execution",
        "idx_namespace_execution_traces_workspace_session_started",
        "idx_resource_samples_sandbox_time",
        "idx_resource_samples_workspace_time",
        "idx_spans_trace_call_index",
        "idx_traces_request",
        "idx_traces_sandbox_started",
        "idx_workspace_snapshots_sandbox",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn table_names(connection: &Connection) -> rusqlite::Result<BTreeSet<String>> {
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

fn index_names(connection: &Connection) -> rusqlite::Result<BTreeSet<String>> {
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

fn column_names(connection: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<Result<_, _>>()
}

fn migration_count(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get(0)
    })
}

fn row_count(connection: &Connection, table: &str) -> rusqlite::Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection.query_row(&sql, [], |row| row.get(0))
}

struct ResourceSampleRow {
    workspace_id: Option<String>,
    cgroup_available: bool,
    cgroup_error: Option<String>,
}

fn resource_sample_rows(
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

fn workspace_snapshot(workspace_id: &str, sampled_at_unix_ms: i64) -> WorkspaceSnapshotRecord {
    WorkspaceSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        workspace_id: workspace_id.to_owned(),
        state: "active".to_owned(),
        remount_state: Some("active".to_owned()),
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

fn execution_snapshot(
    execution_id: &str,
    workspace_id: &str,
    sampled_at_unix_ms: i64,
) -> ExecutionSnapshotRecord {
    ExecutionSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        workspace_id: workspace_id.to_owned(),
        execution_id: execution_id.to_owned(),
        execution_kind: "command".to_owned(),
        operation: Some("exec_command".to_owned()),
        command_session_id: Some(execution_id.to_owned()),
        command: Some("printf ok".to_owned()),
        lifecycle_state: "running".to_owned(),
        finalization_state: "not_started".to_owned(),
        workspace_ownership: Some("existing_session".to_owned()),
        started_at_unix_ms: None,
        wall_time_ms: Some(12.5),
        process_group_id: Some(1234),
        transcript_path: Some(format!("/tmp/{execution_id}/transcript.log")),
        sampled_at_unix_ms,
        error_message: None,
    }
}

fn namespace_execution_snapshot(
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

fn resource_sample(
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
        memory_current_bytes: None,
        memory_max_bytes: None,
        memory_max_unlimited: None,
        disk_upperdir_bytes: Some(4096),
        disk_file_count: Some(1),
        disk_dir_count: Some(1),
        disk_symlink_count: Some(0),
        disk_truncated: Some(false),
        disk_read_error_count: Some(0),
        disk_first_error_path: None,
    }
}
