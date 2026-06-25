use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::observability::DaemonObservability;
use crate::server::{SandboxDaemonServer, ServerConfig};
use rusqlite::{Connection, OptionalExtension};
use sandbox_observability::{
    NamespaceExecutionSnapshotRecord, ObservabilityPaths, ResourceSampleRecord,
    SandboxSnapshotRecord, StoreError, WorkspaceSnapshotRecord,
};
use sandbox_runtime::{
    NamespaceExecutionId, RuntimeNamespaceExecutionSnapshot, RuntimeObservabilitySnapshot,
    RuntimeWorkspaceSnapshot, WorkspaceProfile, WorkspaceSessionId,
};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

/// Snapshot-capture and fault-injection helpers the integration suite drives
/// against the path-included `DaemonObservability`; they stay in the test crate
/// so production carries no test-only surface.
trait DaemonObservabilityTestExt {
    fn collect_runtime_snapshot_for_test(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
    ) -> Result<(), StoreError>;

    fn collect_runtime_snapshot_at_for_test(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
        sampled_at_unix_ms: i64,
    ) -> Result<(), StoreError>;
}

impl DaemonObservabilityTestExt for DaemonObservability {
    fn collect_runtime_snapshot_for_test(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
    ) -> Result<(), StoreError> {
        self.write_snapshot(config, snapshot, test_unix_ms(), false)
    }

    fn collect_runtime_snapshot_at_for_test(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
        sampled_at_unix_ms: i64,
    ) -> Result<(), StoreError> {
        self.write_snapshot(config, snapshot, sampled_at_unix_ms, false)
    }
}

struct TestObservabilityStore {
    database_path: PathBuf,
}

impl TestObservabilityStore {
    fn connection(&self) -> rusqlite::Result<Connection> {
        Connection::open(&self.database_path)
    }

    fn sandbox_snapshot_for_test(
        &self,
        sandbox_id: &str,
    ) -> TestResult<Option<SandboxSnapshotRecord>> {
        let connection = self.connection()?;
        Ok(connection
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
            .optional()?)
    }

    fn workspace_snapshots_for_test(
        &self,
        sandbox_id: &str,
    ) -> TestResult<Vec<WorkspaceSnapshotRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT
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
            FROM workspace_snapshots
            WHERE sandbox_id = ?1
            ORDER BY workspace_id",
        )?;
        let rows = statement.query_map([sandbox_id], |row| {
            Ok(WorkspaceSnapshotRecord {
                sandbox_id: row.get(0)?,
                workspace_id: row.get(1)?,
                state: row.get(2)?,
                profile: row.get(3)?,
                workspace_root: row.get(4)?,
                upperdir: row.get(5)?,
                workdir: row.get(6)?,
                namespace_fd_count: row.get(7)?,
                base_manifest_version: row.get(8)?,
                base_root_hash: row.get(9)?,
                layer_count: row.get(10)?,
                sampled_at_unix_ms: row.get(11)?,
                error_message: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn namespace_execution_snapshots_for_test(
        &self,
        sandbox_id: &str,
    ) -> TestResult<Vec<NamespaceExecutionSnapshotRecord>> {
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
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn resource_samples_for_test(&self, sandbox_id: &str) -> TestResult<Vec<ResourceSampleRecord>> {
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
                cpu_usage_delta_usec: row.get(8)?,
                sample_delta_ms: row.get(9)?,
                memory_current_bytes: row.get(10)?,
                memory_current_delta_bytes: row.get(11)?,
                memory_max_bytes: row.get(12)?,
                memory_max_unlimited: row.get::<_, Option<i64>>(13)?.map(|value| value != 0),
                disk_upperdir_bytes: row.get(14)?,
                disk_upperdir_delta_bytes: row.get(15)?,
                disk_file_count: row.get(16)?,
                disk_dir_count: row.get(17)?,
                disk_symlink_count: row.get(18)?,
                disk_truncated: row.get::<_, Option<i64>>(19)?.map(|value| value != 0),
                disk_read_error_count: row.get(20)?,
                disk_first_error_path: row.get(21)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

struct SqliteWriteBlocker {
    _connection: Connection,
}

fn test_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[test]
fn observability_collection_writes_namespace_only_live_snapshot() -> TestResult {
    let root = test_root("collects-namespace-only");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = runtime_snapshot(root.join("missing-upperdir"));

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let sandbox = store
        .sandbox_snapshot_for_test("sandbox-1")?
        .expect("sandbox snapshot written");
    assert_eq!(sandbox.state, "ready");
    assert_eq!(
        sandbox.socket_path.as_deref(),
        Some(config.socket_path.to_string_lossy().as_ref())
    );

    let workspaces = store.workspace_snapshots_for_test("sandbox-1")?;
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].workspace_id, "workspace-1");
    assert_eq!(workspaces[0].state, "active");

    let namespace_executions = store.namespace_execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(namespace_executions.len(), 1);
    assert_eq!(
        namespace_executions[0].namespace_execution_id,
        "namespace_execution_1"
    );
    assert_eq!(namespace_executions[0].workspace_session_id, "workspace-1");
    assert_eq!(namespace_executions[0].operation, "exec_command");

    let samples = store.resource_samples_for_test("sandbox-1")?;
    assert_eq!(samples.len(), 2);
    let global = samples
        .iter()
        .find(|sample| sample.workspace_id.is_none())
        .expect("sandbox-global sample written");
    assert!(!global.cgroup_available);
    assert_eq!(
        global.cgroup_error.as_deref(),
        Some("cgroup root unavailable")
    );
    let workspace = samples
        .iter()
        .find(|sample| sample.workspace_id.as_deref() == Some("workspace-1"))
        .expect("workspace sample written");
    assert!(!workspace.cgroup_available);
    assert_eq!(
        workspace.cgroup_error.as_deref(),
        Some("workspace cgroup unavailable")
    );
    assert!(workspace.disk_read_error_count.unwrap_or_default() > 0);
    assert!(workspace.disk_first_error_path.is_some());
    Ok(())
}

#[test]
fn observability_collection_populates_cgroup_counters_from_fixture_paths() -> TestResult {
    let root = test_root("cgroup-counters");
    let sandbox_cgroup = root.join("cgroup-root");
    write_cgroup_fixture(&sandbox_cgroup, 4_096, 8_192, "max")?;
    let workspace_cgroup = sandbox_cgroup.join("workspace-workspace-1");
    write_cgroup_fixture(&workspace_cgroup, 2_048, 4_096, "16384")?;

    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(sandbox_cgroup.clone());
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: vec![RuntimeWorkspaceSnapshot {
            cgroup_path: Some(workspace_cgroup.clone()),
            ..workspace_snapshot("workspace-1", None)
        }],
        active_namespace_executions: Vec::new(),
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let samples = store.resource_samples_for_test("sandbox-1")?;
    let global = samples
        .iter()
        .find(|sample| sample.workspace_id.is_none())
        .expect("sandbox-global sample written");
    assert!(global.cgroup_available);
    assert_eq!(global.cgroup_error, None);
    assert_eq!(global.cpu_usage_usec, Some(4_096));
    assert_eq!(global.memory_current_bytes, Some(8_192));
    assert_eq!(global.memory_max_bytes, None);
    assert_eq!(global.memory_max_unlimited, Some(true));

    let workspace = samples
        .iter()
        .find(|sample| sample.workspace_id.as_deref() == Some("workspace-1"))
        .expect("workspace sample written");
    assert!(workspace.cgroup_available);
    assert_eq!(workspace.cpu_usage_usec, Some(2_048));
    assert_eq!(workspace.memory_current_bytes, Some(4_096));
    assert_eq!(workspace.memory_max_bytes, Some(16_384));
    assert_eq!(workspace.memory_max_unlimited, Some(false));

    assert!(global.cpu_usage_usec >= workspace.cpu_usage_usec);
    Ok(())
}

#[test]
fn observability_collection_computes_resource_deltas_per_scope() -> TestResult {
    let root = test_root("resource-deltas");
    let sandbox_cgroup = root.join("cgroup-root");
    let workspace_cgroup = sandbox_cgroup.join("workspace-workspace-1");
    let upperdir = root.join("upperdir");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::write(upperdir.join("one.txt"), b"1")?;
    write_cgroup_fixture(&sandbox_cgroup, 4_000, 8_000, "max")?;
    write_cgroup_fixture(&workspace_cgroup, 2_000, 4_000, "16384")?;

    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(sandbox_cgroup.clone());
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    let snapshot = || RuntimeObservabilitySnapshot {
        workspaces: vec![RuntimeWorkspaceSnapshot {
            cgroup_path: Some(workspace_cgroup.clone()),
            ..workspace_snapshot("workspace-1", Some(upperdir.clone()))
        }],
        active_namespace_executions: Vec::new(),
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_at_for_test(&config, snapshot(), 1_000)?;
    std::fs::write(upperdir.join("two.txt"), b"22")?;
    write_cgroup_fixture(&sandbox_cgroup, 4_600, 7_500, "max")?;
    write_cgroup_fixture(&workspace_cgroup, 2_300, 4_500, "16384")?;
    observability.write_snapshot(&config, snapshot(), 1_250, true)?;

    let store = store_for_config(&config)?;
    let samples = store.resource_samples_for_test("sandbox-1")?;
    let latest_global = samples
        .iter()
        .rev()
        .find(|sample| sample.workspace_id.is_none())
        .expect("latest sandbox-global sample");
    assert_eq!(latest_global.sample_delta_ms, Some(250));
    assert_eq!(latest_global.cpu_usage_delta_usec, Some(600));
    assert_eq!(latest_global.memory_current_delta_bytes, Some(-500));

    let latest_workspace = samples
        .iter()
        .rev()
        .find(|sample| sample.workspace_id.as_deref() == Some("workspace-1"))
        .expect("latest workspace sample");
    assert_eq!(latest_workspace.sample_delta_ms, Some(250));
    assert_eq!(latest_workspace.cpu_usage_delta_usec, Some(300));
    assert_eq!(latest_workspace.memory_current_delta_bytes, Some(500));
    assert_eq!(latest_workspace.disk_upperdir_bytes, Some(3));
    assert_eq!(latest_workspace.disk_upperdir_delta_bytes, Some(2));
    Ok(())
}

fn write_cgroup_fixture(
    dir: &Path,
    cpu_usage_usec: u64,
    memory_current_bytes: u64,
    memory_max: &str,
) -> TestResult {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("cpu.stat"), format!("usage_usec {cpu_usage_usec}\n"))?;
    std::fs::write(
        dir.join("memory.current"),
        format!("{memory_current_bytes}\n"),
    )?;
    std::fs::write(dir.join("memory.max"), format!("{memory_max}\n"))?;
    Ok(())
}

#[test]
fn namespace_execution_snapshots_do_not_persist_command_payload_data() -> TestResult {
    let root = test_root("namespace-snapshot-no-command-payload");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: Vec::new(),
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
        }],
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let snapshots = store.namespace_execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    let values = [
        snapshot.sandbox_id.as_str(),
        snapshot.namespace_execution_id.as_str(),
        snapshot.workspace_session_id.as_str(),
        snapshot.operation.as_str(),
        snapshot.lifecycle_state.as_str(),
        snapshot.error_message.as_deref().unwrap_or_default(),
    ];
    for forbidden in [
        "SECRET_COMMAND_TEXT",
        "SECRET_TRANSCRIPT_PATH",
        "SECRET_TRANSCRIPT_CONTENT",
        "SECRET_STDIN",
        "SECRET_STDOUT",
        "SECRET_STDERR",
        "SECRET_ENV",
    ] {
        assert!(
            values.iter().all(|value| !value.contains(forbidden)),
            "namespace execution snapshot unexpectedly contained {forbidden}"
        );
    }
    Ok(())
}

#[test]
fn observability_collection_bounds_rows_and_keeps_valid_rows() -> TestResult {
    let root = test_root("bounds-rows");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let valid_upperdir = root.join("valid-upperdir");
    std::fs::create_dir_all(&valid_upperdir)?;
    std::fs::write(valid_upperdir.join("ok.txt"), b"ok")?;

    let long_workspace_id = "workspace-id-that-is-too-long".repeat(20);
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: vec![
            workspace_snapshot("workspace-1", Some(valid_upperdir)),
            workspace_snapshot(&long_workspace_id, Some(root.join("missing-upperdir"))),
            workspace_snapshot("", None),
        ],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".repeat(20),
        }],
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let sandbox = store
        .sandbox_snapshot_for_test("sandbox-1")?
        .expect("sandbox snapshot written");
    assert_eq!(sandbox.state, "ready");
    assert!(sandbox
        .error_message
        .as_deref()
        .is_some_and(|message| message.contains("workspace_id is empty")));
    let workspaces = store.workspace_snapshots_for_test("sandbox-1")?;
    assert!(workspaces
        .iter()
        .any(|workspace| workspace.workspace_id == "workspace-1"));
    assert!(workspaces
        .iter()
        .all(|workspace| workspace.workspace_id.len() <= 256));
    assert!(workspaces
        .iter()
        .all(|workspace| !workspace.workspace_id.is_empty()));

    let namespace_executions = store.namespace_execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(namespace_executions.len(), 1);
    assert!(namespace_executions[0].operation.len() <= 128);
    Ok(())
}

#[test]
fn disk_samples_are_cached_until_tests_force_refresh_and_can_truncate() -> TestResult {
    let root = test_root("disk-cache");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let upperdir = root.join("upperdir");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::write(upperdir.join("one.txt"), b"1")?;

    observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir.clone()))],
            active_namespace_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;
    std::fs::write(upperdir.join("two.txt"), b"2")?;
    observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir.clone()))],
            active_namespace_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;

    let store = store_for_config(&config)?;
    let cached = latest_workspace_sample(&store, "sandbox-1", "workspace-1")?;
    assert_eq!(cached.disk_file_count, Some(1));
    assert_eq!(cached.disk_truncated, Some(false));

    let refreshed_observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    refreshed_observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir.clone()))],
            active_namespace_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;
    let refreshed = latest_workspace_sample(&store, "sandbox-1", "workspace-1")?;
    assert_eq!(refreshed.disk_file_count, Some(2));

    let large_upperdir = root.join("large-upperdir");
    std::fs::create_dir_all(&large_upperdir)?;
    for index in 0..1030 {
        std::fs::write(large_upperdir.join(format!("file-{index}")), b"x")?;
    }
    refreshed_observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot(
                "workspace-large",
                Some(large_upperdir.clone()),
            )],
            active_namespace_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;
    let truncated = latest_workspace_sample(&store, "sandbox-1", "workspace-large")?;
    assert_eq!(truncated.disk_truncated, Some(true));
    Ok(())
}

#[test]
fn observability_is_disabled_when_sandbox_id_is_missing() {
    let root = test_root("missing-sandbox-id");
    let config = server_config(&root, None);

    assert!(DaemonObservability::from_config(&config).is_none());
}

#[tokio::test]
async fn private_observability_snapshot_dispatch_returns_summary_tree() -> TestResult {
    let root = test_root("private-snapshot-summary");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let observability = server
        .observability
        .as_ref()
        .expect("sandbox id enables observability");
    let mut snapshot = runtime_snapshot(root.join("missing-upperdir"));
    snapshot
        .partial_errors
        .push("partial workspace projection failed".to_owned());
    observability.collect_runtime_snapshot_for_test(&server.config, snapshot)?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::server::dispatch::PRIVATE_OBSERVABILITY_SNAPSHOT_OP,
                "req-private-snapshot",
                json!({
                    "resource_window_ms": 60_000,
                }),
            )?,
            false,
        )
        .await;

    assert_eq!(response["sandbox_id"], "sandbox-1");
    assert_eq!(response["lifecycle_state"], "ready");
    assert_eq!(response["availability"], "partial");
    assert_eq!(
        response["errors"][0],
        "partial workspace projection failed"
    );
    assert_eq!(response["workspaces"][0]["workspace_id"], "workspace-1");
    assert_eq!(
        response["workspaces"][0]["active_namespace_executions"][0]["namespace_execution_id"],
        "namespace_execution_1"
    );
    assert_eq!(
        response["resources"]["history"]
            .as_array()
            .expect("sandbox resource history loaded")
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn missing_sandbox_id_disables_trace_persistence_without_failing_request() -> TestResult {
    let root = test_root("trace-missing-sandbox-id");
    let server = daemon_server(&root, None)?;
    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;

    let response = server
        .dispatch_bytes(request_bytes("missing_op", "req-disabled", json!({}))?, false)
        .await;

    assert_eq!(response["error"]["kind"], "unknown_op");
    assert!(!paths.database_path().exists());
    Ok(())
}

#[tokio::test]
async fn observability_store_failure_does_not_alter_operation_response() -> TestResult {
    let root = test_root("trace-store-failure");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let _write_blocker = block_sqlite_writes_for_test(&server.config)?;

    let response = server
        .dispatch_bytes(request_bytes("missing_op", "req-store-failure", json!({}))?, false)
        .await;

    assert_eq!(response["error"]["kind"], "unknown_op");
    assert_eq!(response["error"]["message"], "unknown operation");
    Ok(())
}

#[tokio::test]
async fn observability_write_errors_do_not_change_operation_responses() -> TestResult {
    let root = test_root("write-error-isolated");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let observability = server
        .observability
        .as_ref()
        .expect("sandbox id enables observability");
    let _write_blocker = block_sqlite_writes_for_test(&server.config)?;

    let collect_error = observability
        .collect(&server.config, server.operations.as_ref())
        .expect_err("forced sqlite write failure is observed before dispatch");
    assert!(
        collect_error.to_string().contains("sqlite"),
        "{collect_error}"
    );

    let request = serde_json::json!({
        "op": "unknown_runtime_op",
        "request_id": "req-1",
        "scope": {
            "kind": "sandbox",
            "sandbox_id": "sandbox-1"
        },
        "args": {},
    });
    let response = server
        .dispatch_bytes(serde_json::to_vec(&request)?, false)
        .await;

    assert_eq!(
        response,
        sandbox_protocol::Response::unknown_op().into_json_value()
    );
    Ok(())
}

fn daemon_server(root: &Path, sandbox_id: Option<&str>) -> TestResult<SandboxDaemonServer> {
    let config = server_config(root, sandbox_id);
    Ok(SandboxDaemonServer::new_with_runtime_config(
        config,
        runtime_config(root)?,
    ))
}

fn request_bytes(op: &str, request_id: &str, args: Value) -> TestResult<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "op": op,
        "request_id": request_id,
        "scope": {
            "kind": "sandbox",
            "sandbox_id": "sandbox-1",
        },
        "args": args,
    }))?)
}

fn runtime_snapshot(missing_upperdir: PathBuf) -> RuntimeObservabilitySnapshot {
    RuntimeObservabilitySnapshot {
        workspaces: vec![workspace_snapshot("workspace-1", Some(missing_upperdir))],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
        }],
        partial_errors: Vec::new(),
    }
}

fn workspace_snapshot(workspace_id: &str, upperdir: Option<PathBuf>) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(workspace_id.to_owned()),
        profile: WorkspaceProfile::HostCompatible,
        workspace_root: PathBuf::from("/workspace").join(workspace_id),
        upperdir,
        workdir: Some(PathBuf::from("/workspace").join(workspace_id).join("work")),
        namespace_fd_count: Some(3),
        base_manifest_version: Some(1),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(1),
        cgroup_path: None,
    }
}

fn store_for_config(config: &ServerConfig) -> TestResult<TestObservabilityStore> {
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    Ok(TestObservabilityStore {
        database_path: paths.database_path().to_path_buf(),
    })
}

fn latest_workspace_sample(
    store: &TestObservabilityStore,
    sandbox_id: &str,
    workspace_id: &str,
) -> TestResult<sandbox_observability::ResourceSampleRecord> {
    store
        .resource_samples_for_test(sandbox_id)?
        .into_iter()
        .rfind(|sample| sample.workspace_id.as_deref() == Some(workspace_id))
        .ok_or_else(|| format!("missing resource sample for {workspace_id}").into())
}

fn block_sqlite_writes_for_test(config: &ServerConfig) -> TestResult<SqliteWriteBlocker> {
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    let connection = Connection::open(paths.database_path())?;
    connection.execute_batch("BEGIN IMMEDIATE")?;
    Ok(SqliteWriteBlocker {
        _connection: connection,
    })
}

fn server_config(root: &Path, sandbox_id: Option<&str>) -> ServerConfig {
    ServerConfig {
        socket_path: root.join("runtime.sock"),
        pid_path: root.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        auth_token: None,
        sandbox_id: sandbox_id.map(str::to_owned),
        cgroup_root: None,
    }
}

fn runtime_config(root: &Path) -> TestResult<sandbox_runtime::SandboxRuntimeConfig> {
    let layer_stack_root = root.join("layer-stack");
    let workspace_root = root.join("runtime-workspace");
    std::fs::create_dir_all(&workspace_root)?;
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)?;
    Ok(sandbox_runtime::SandboxRuntimeConfig {
        cgroup_root: None,
        workspace: sandbox_runtime::WorkspaceRuntimeConfig {
            workspace_root,
            layer_stack_root,
            scratch_root: root.join("workspace-scratch"),
            caps: sandbox_runtime::WorkspaceResourceCaps {
                upperdir_bytes: 1_073_741_824,
                memavail_fraction: 0.5,
                setup_timeout_s: 30.0,
                exit_grace_s: 0.25,
                rfc1918_egress: sandbox_runtime::Rfc1918Egress::Allow,
            },
        },
        command: sandbox_runtime::CommandRuntimeConfig {
            scratch_root: root.join("command-scratch"),
        },
    })
}

fn test_root(label: &str) -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "sandbox-daemon-observability-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create test root");
    root
}
