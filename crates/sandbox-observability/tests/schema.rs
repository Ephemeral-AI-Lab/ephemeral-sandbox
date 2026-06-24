mod support;

use rusqlite::Connection;
use sandbox_observability::{
    NamespaceExecutionTraceRecord, ObservabilitySnapshotReadOptions, ObservabilityStore,
    SandboxSnapshotRecord, SpanRecord, StoreError, TraceRecord,
};
use support::{
    allowed_indexes, allowed_tables, column_names, current_unix_ms, index_names, migration_count,
    namespace_execution_snapshot, resource_sample, resource_sample_rows, row_count, table_names,
    test_paths, workspace_snapshot, TestResult,
};

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
    for forbidden_column in [
        "command_session_id",
        "command",
        "transcript_path",
        "stdin",
        "stdout",
        "stderr",
        "environment",
        "output",
        "execution_kind",
    ] {
        assert!(
            !namespace_snapshot_columns.contains(forbidden_column),
            "namespace execution snapshots unexpectedly include {forbidden_column}"
        );
    }
    let namespace_trace_columns = column_names(&connection, "namespace_execution_traces")?;
    assert!(namespace_trace_columns.contains("namespace_execution_id"));
    assert!(namespace_trace_columns.contains("workspace_session_id"));
    assert!(namespace_trace_columns.contains("exit_code"));
    assert_eq!(migration_count(&connection)?, 5);
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
fn schema_migration_checksums_are_recorded() -> TestResult {
    let (_dir, paths) = test_paths("schema-checksums-recorded")?;
    let store = ObservabilityStore::open(&paths)?;
    drop(store);

    let connection = Connection::open(paths.database_path())?;
    let mut statement =
        connection.prepare("SELECT checksum FROM schema_migrations ORDER BY version")?;
    let checksums = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    assert_eq!(checksums.len(), 5);
    assert!(
        checksums
            .iter()
            .all(|checksum| checksum.starts_with("fnv1a64:")),
        "{checksums:?}"
    );
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
fn namespace_execution_snapshot_and_completed_trace_use_typed_tables() -> TestResult {
    let (_dir, paths) = test_paths("namespace-execution")?;
    let store = ObservabilityStore::open(&paths)?;

    store.replace_namespace_execution_snapshots(
        "sandbox-1",
        &[
            namespace_execution_snapshot("namespace_execution_1", "workspace-1", 1_000),
            namespace_execution_snapshot("namespace_execution_2", "workspace-1", 1_000),
        ],
    )?;
    store.replace_namespace_execution_snapshots(
        "sandbox-1",
        &[namespace_execution_snapshot(
            "namespace_execution_2",
            "workspace-1",
            1_000,
        )],
    )?;
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

#[test]
fn aggregate_snapshot_read_returns_latest_resources_per_scope() -> TestResult {
    let (_dir, paths) = test_paths("aggregate-latest-resources")?;
    let store = ObservabilityStore::open(&paths)?;

    store.upsert_sandbox_snapshot(&SandboxSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        state: "ready".to_owned(),
        workspace_root: None,
        daemon_runtime_dir: Some("/tmp/daemon".to_owned()),
        socket_path: Some("/tmp/daemon/runtime.sock".to_owned()),
        pid_path: Some("/tmp/daemon/runtime.pid".to_owned()),
        daemon_pid: Some(42),
        sampled_at_unix_ms: 2_000,
        error_message: None,
    })?;
    store.upsert_workspace_snapshots(
        "sandbox-1",
        &[
            workspace_snapshot("workspace-1", 1_000),
            workspace_snapshot("workspace-2", 1_000),
        ],
    )?;
    store.insert_resource_samples(&[
        resource_sample("global-old", None, 1_000),
        resource_sample("global-new", None, 2_000),
        resource_sample("workspace-1-old", Some("workspace-1"), 1_100),
        resource_sample("workspace-1-new", Some("workspace-1"), 2_100),
        resource_sample("workspace-2-only", Some("workspace-2"), 1_500),
        resource_sample("workspace-orphan-new", Some("workspace-3"), 3_000),
    ])?;

    let rows = store.read_observability_snapshot(
        "sandbox-1",
        &ObservabilitySnapshotReadOptions {
            include_recent_traces: false,
            trace_limit: 20,
            resource_window_ms: None,
        },
    )?;

    assert!(rows.sandbox.is_some());
    assert_eq!(rows.workspaces.len(), 2);
    assert_eq!(
        rows.latest_resources
            .iter()
            .map(|sample| (sample.workspace_id.as_deref(), sample.sampled_at_unix_ms))
            .collect::<Vec<_>>(),
        [
            (None, 2_000),
            (Some("workspace-1"), 2_100),
            (Some("workspace-2"), 1_500)
        ]
    );
    assert!(rows.resource_history.is_empty());
    assert!(rows.recent_request_traces.is_empty());
    assert!(rows.recent_namespace_traces.is_empty());
    Ok(())
}

#[test]
fn aggregate_snapshot_read_history_and_traces_are_opt_in() -> TestResult {
    let (_dir, paths) = test_paths("aggregate-history-traces")?;
    let store = ObservabilityStore::open(&paths)?;
    let now = current_unix_ms()?;

    store.upsert_sandbox_snapshot(&SandboxSnapshotRecord {
        sandbox_id: "sandbox-1".to_owned(),
        state: "ready".to_owned(),
        workspace_root: None,
        daemon_runtime_dir: None,
        socket_path: None,
        pid_path: None,
        daemon_pid: None,
        sampled_at_unix_ms: now,
        error_message: None,
    })?;
    store.upsert_workspace_snapshots("sandbox-1", &[workspace_snapshot("workspace-1", now)])?;
    store.insert_resource_samples(&[
        resource_sample("history-old", None, now.saturating_sub(10_000)),
        resource_sample("history-recent", None, now.saturating_sub(100)),
    ])?;
    store.insert_trace(
        &TraceRecord {
            trace_id: "trace-1".to_owned(),
            kind: "request".to_owned(),
            status: "ok".to_owned(),
            sandbox_id: "sandbox-1".to_owned(),
            operation: "exec_command".to_owned(),
            request_id: Some("request-1".to_owned()),
            origin_request_id: None,
            workspace_id: Some("workspace-1".to_owned()),
            command_session_id: Some("command-session-secret".to_owned()),
            started_at_unix_ms: now.saturating_sub(50),
            finished_at_unix_ms: Some(now.saturating_sub(25)),
            duration_ms: Some(25.0),
            error_kind: None,
            error_message: None,
        },
        &[SpanRecord {
            span_id: "span-1".to_owned(),
            trace_id: "trace-1".to_owned(),
            parent_span_id: None,
            method_name: "secret.span.method".to_owned(),
            call_index: 0,
            status: "ok".to_owned(),
            started_at_unix_ms: now.saturating_sub(50),
            finished_at_unix_ms: Some(now.saturating_sub(25)),
            duration_ms: Some(25.0),
            error_kind: None,
            error_message: None,
        }],
    )?;
    store.insert_namespace_execution_trace(&NamespaceExecutionTraceRecord {
        trace_id: "namespace_execution:namespace-1".to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        namespace_execution_id: "namespace-1".to_owned(),
        workspace_session_id: "workspace-1".to_owned(),
        operation: "exec_command".to_owned(),
        request_id: Some("request-2".to_owned()),
        status: "ok".to_owned(),
        exit_code: Some(0),
        started_at_unix_ms: now.saturating_sub(40),
        finished_at_unix_ms: now.saturating_sub(10),
        duration_ms: 30.0,
        error_kind: None,
        error_message: None,
    })?;

    let default_rows = store.read_observability_snapshot(
        "sandbox-1",
        &ObservabilitySnapshotReadOptions {
            include_recent_traces: false,
            trace_limit: 20,
            resource_window_ms: None,
        },
    )?;
    assert!(default_rows.resource_history.is_empty());
    assert!(default_rows.recent_request_traces.is_empty());
    assert!(default_rows.recent_namespace_traces.is_empty());

    let requested_rows = store.read_observability_snapshot(
        "sandbox-1",
        &ObservabilitySnapshotReadOptions {
            include_recent_traces: true,
            trace_limit: 20,
            resource_window_ms: Some(1_000),
        },
    )?;
    assert_eq!(requested_rows.resource_history.len(), 1);
    assert_eq!(
        requested_rows.resource_history[0].sampled_at_unix_ms,
        now.saturating_sub(100)
    );
    assert_eq!(requested_rows.recent_request_traces.len(), 1);
    assert_eq!(requested_rows.recent_namespace_traces.len(), 1);
    Ok(())
}
