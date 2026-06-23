use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::observability::DaemonObservability;
use crate::server::{SandboxDaemonServer, ServerConfig};
use sandbox_observability::{ObservabilityPaths, ObservabilityStore, SpanRecord, TraceRecord};
use sandbox_runtime::command::CommandSessionId;
use sandbox_runtime::{CompletedOperationSpan, CompletedOperationTrace, WorkspaceSessionId};
use sandbox_runtime::{
    RuntimeExecutionSnapshot, RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot,
    WorkspaceProfile,
};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[test]
fn observability_collection_writes_phase2_live_snapshot() -> TestResult {
    let root = test_root("collects-phase2");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = runtime_snapshot(root.join("missing-upperdir"));

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    let store = ObservabilityStore::open(&paths)?;
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
    assert_eq!(workspaces[0].remount_state.as_deref(), Some("active"));

    let executions = store.execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].execution_id, "cmd_1");
    assert_eq!(executions[0].execution_kind, "command");
    assert_eq!(executions[0].workspace_id, "workspace-1");

    let samples = store.resource_samples_for_test("sandbox-1")?;
    assert_eq!(samples.len(), 2);
    let global = samples
        .iter()
        .find(|sample| sample.workspace_id.is_none())
        .expect("sandbox-global sample written");
    assert!(!global.cgroup_available);
    assert_eq!(
        global.cgroup_error.as_deref(),
        Some("cgroup path unavailable")
    );
    let workspace = samples
        .iter()
        .find(|sample| sample.workspace_id.as_deref() == Some("workspace-1"))
        .expect("workspace sample written");
    assert!(!workspace.cgroup_available);
    assert!(workspace.disk_read_error_count.unwrap_or_default() > 0);
    assert!(workspace.disk_first_error_path.is_some());
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
        active_executions: vec![RuntimeExecutionSnapshot {
            execution_id: "cmd_1".to_owned(),
            execution_kind: "command".repeat(20),
            operation: Some("exec_command".repeat(20)),
            command_session_id: Some(CommandSessionId("cmd_1".to_owned())),
            workspace_id: WorkspaceSessionId("workspace-1".to_owned()),
            command: Some("x".repeat(5000)),
            lifecycle_state: "running".to_owned(),
            finalization_state: "not_started".to_owned(),
            workspace_ownership: "existing_session".to_owned(),
            started_at_unix_ms: None,
            wall_time_ms: Some(10.0),
            transcript_path: Some(PathBuf::from("/tmp/transcript.log")),
            process_group_id: Some(1234),
        }],
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let sandbox = store
        .sandbox_snapshot_for_test("sandbox-1")?
        .expect("sandbox snapshot written");
    assert_eq!(sandbox.state, "unavailable");
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

    let executions = store.execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(executions.len(), 1);
    assert!(executions[0].execution_kind.len() <= 64);
    assert!(executions[0].operation.as_ref().is_some_and(|value| value.len() <= 128));
    assert!(executions[0].command.as_ref().is_some_and(|value| value.len() <= 4096));
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
            active_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;
    std::fs::write(upperdir.join("two.txt"), b"2")?;
    observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir.clone()))],
            active_executions: Vec::new(),
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
            active_executions: Vec::new(),
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
            active_executions: Vec::new(),
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

#[test]
fn completed_operation_trace_maps_and_persists_success() -> TestResult {
    let root = test_root("trace-success");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-success".to_owned(),
        "exec_command".to_owned(),
        &json!({
            "status": "completed",
            "output": "SECRET_OUTPUT",
            "transcript": "SECRET_TRANSCRIPT",
        }),
        completed_trace(&[
            (None, "dispatch_operation", 0),
            (Some(0), "exec_command::dispatch", 1),
            (Some(1), "CommandOperationService::exec_command", 2),
        ]),
    )?;

    let store = store_for_config(&config)?;
    let trace = trace_for(&store, "request:req-success")?;
    assert_eq!(trace.kind, "request");
    assert_eq!(trace.status, "ok");
    assert_eq!(trace.sandbox_id, "sandbox-1");
    assert_eq!(trace.operation, "exec_command");
    assert_eq!(trace.request_id.as_deref(), Some("req-success"));
    assert!(trace.error_kind.is_none());

    let spans = store.spans_for_test("request:req-success")?;
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].span_id, "request:req-success:span:0");
    assert_eq!(spans[1].parent_span_id.as_deref(), Some("request:req-success:span:0"));
    assert_eq!(spans[2].parent_span_id.as_deref(), Some("request:req-success:span:1"));
    assert_no_trace_text(&trace, &spans, "SECRET_OUTPUT");
    assert_no_trace_text(&trace, &spans, "SECRET_TRANSCRIPT");
    Ok(())
}

#[test]
fn completed_operation_trace_marks_deepest_span_on_error() -> TestResult {
    let root = test_root("trace-error");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-error".to_owned(),
        "exec_command".to_owned(),
        &json!({
            "error": {
                "kind": "operation_failed",
                "message": "command failed",
                "details": { "output": "SECRET_OUTPUT" }
            }
        }),
        completed_trace(&[
            (None, "dispatch_operation", 0),
            (Some(0), "exec_command::dispatch", 1),
            (Some(1), "CommandOperationService::exec_command", 2),
        ]),
    )?;

    let store = store_for_config(&config)?;
    let trace = trace_for(&store, "request:req-error")?;
    assert_eq!(trace.status, "error");
    assert_eq!(trace.error_kind.as_deref(), Some("operation_failed"));
    assert_eq!(trace.error_message.as_deref(), Some("command failed"));
    let spans = store.spans_for_test("request:req-error")?;
    assert_eq!(spans[2].status, "error");
    assert_eq!(spans[2].error_kind.as_deref(), Some("operation_failed"));
    assert_eq!(spans[2].error_message.as_deref(), Some("command failed"));
    assert_no_trace_text(&trace, &spans, "SECRET_OUTPUT");
    Ok(())
}

#[test]
fn completed_operation_trace_persists_long_distinct_request_ids() -> TestResult {
    let root = test_root("trace-long-request-id");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let shared_prefix = "request-id-with-shared-prefix-".repeat(20);

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        format!("{shared_prefix}a"),
        "exec_command".to_owned(),
        &json!({ "status": "ok" }),
        completed_trace(&[(None, "dispatch_operation", 0)]),
    )?;
    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        format!("{shared_prefix}b"),
        "exec_command".to_owned(),
        &json!({ "status": "ok" }),
        completed_trace(&[(None, "dispatch_operation", 0)]),
    )?;

    Ok(())
}

#[tokio::test]
async fn unknown_operation_trace_persistence() -> TestResult {
    let root = test_root("trace-unknown-op");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(request_bytes("missing_op", "req-unknown", json!({}))?, false)
        .await;

    assert_eq!(response["error"]["kind"], "unknown_op");
    let store = store_for_config(&server.config)?;
    let trace = trace_for(&store, "request:req-unknown")?;
    assert_eq!(trace.status, "error");
    assert_eq!(trace.operation, "missing_op");
    assert_eq!(trace.error_kind.as_deref(), Some("unknown_op"));
    let spans = store.spans_for_test("request:req-unknown")?;
    assert_eq!(span_names(&spans), vec!["dispatch_operation"]);
    assert_eq!(spans[0].status, "error");
    Ok(())
}

#[tokio::test]
async fn operation_argument_parse_error_trace_persistence() -> TestResult {
    let root = test_root("trace-parse-error");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(request_bytes("exec_command", "req-parse", json!({}))?, false)
        .await;

    assert_eq!(response["error"]["kind"], "invalid_request");
    let store = store_for_config(&server.config)?;
    let spans = store.spans_for_test("request:req-parse")?;
    assert_eq!(span_names(&spans), vec!["dispatch_operation", "exec_command::dispatch"]);
    assert_eq!(spans[1].status, "error");
    assert_eq!(spans[1].error_kind.as_deref(), Some("invalid_request"));
    Ok(())
}

#[tokio::test]
async fn operation_service_error_trace_persistence() -> TestResult {
    let root = test_root("trace-service-error");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(
            request_bytes("exec_command", "req-service", json!({ "cmd": "   " }))?,
            false,
        )
        .await;

    assert_eq!(response["error"]["kind"], "operation_failed");
    let store = store_for_config(&server.config)?;
    let spans = store.spans_for_test("request:req-service")?;
    assert_eq!(
        span_names(&spans),
        vec![
            "dispatch_operation",
            "exec_command::dispatch",
            "CommandOperationService::exec_command",
        ]
    );
    assert_eq!(spans[2].status, "error");
    assert_eq!(spans[2].error_kind.as_deref(), Some("operation_failed"));
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
    server
        .observability
        .as_ref()
        .expect("observability enabled")
        .force_sqlite_write_errors_for_test()?;

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
    let config = server_config(&root, Some("sandbox-1"));
    let operations = Arc::new(runtime_operations(&root)?);
    let server = SandboxDaemonServer::new(config, operations);
    let observability = server
        .observability
        .as_ref()
        .expect("sandbox id enables observability");
    observability.force_sqlite_write_errors_for_test()?;

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

fn completed_trace(spans: &[(Option<i64>, &'static str, i64)]) -> CompletedOperationTrace {
    CompletedOperationTrace {
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: 1_050,
        duration_ms: 50.0,
        spans: spans
            .iter()
            .map(|(parent_call_index, method_name, call_index)| CompletedOperationSpan {
                parent_call_index: *parent_call_index,
                method_name,
                call_index: *call_index,
                status: "ok",
                started_at_unix_ms: 1_000 + *call_index,
                finished_at_unix_ms: 1_010 + *call_index,
                duration_ms: 10.0,
            })
            .collect(),
    }
}

fn daemon_server(root: &Path, sandbox_id: Option<&str>) -> TestResult<SandboxDaemonServer> {
    let config = server_config(root, sandbox_id);
    let operations = Arc::new(runtime_operations(root)?);
    Ok(SandboxDaemonServer::new(config, operations))
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

fn trace_for(store: &ObservabilityStore, trace_id: &str) -> TestResult<TraceRecord> {
    store
        .trace_for_test(trace_id)?
        .ok_or_else(|| format!("missing trace {trace_id}").into())
}

fn span_names(spans: &[SpanRecord]) -> Vec<&str> {
    spans
        .iter()
        .map(|span| span.method_name.as_str())
        .collect()
}

fn assert_no_trace_text(trace: &TraceRecord, spans: &[SpanRecord], forbidden: &str) {
    let mut values = vec![
        trace.trace_id.as_str(),
        trace.kind.as_str(),
        trace.status.as_str(),
        trace.sandbox_id.as_str(),
        trace.operation.as_str(),
    ];
    values.extend(trace.request_id.as_deref());
    values.extend(trace.error_kind.as_deref());
    values.extend(trace.error_message.as_deref());
    for span in spans {
        values.push(span.span_id.as_str());
        values.push(span.trace_id.as_str());
        values.extend(span.parent_span_id.as_deref());
        values.push(span.method_name.as_str());
        values.push(span.status.as_str());
        values.extend(span.error_kind.as_deref());
        values.extend(span.error_message.as_deref());
    }
    assert!(
        values.iter().all(|value| !value.contains(forbidden)),
        "trace rows unexpectedly contained {forbidden}"
    );
}

fn runtime_snapshot(missing_upperdir: PathBuf) -> RuntimeObservabilitySnapshot {
    RuntimeObservabilitySnapshot {
        workspaces: vec![workspace_snapshot("workspace-1", Some(missing_upperdir))],
        active_executions: vec![RuntimeExecutionSnapshot {
            execution_id: "cmd_1".to_owned(),
            execution_kind: "command".to_owned(),
            operation: Some("exec_command".to_owned()),
            command_session_id: Some(CommandSessionId("cmd_1".to_owned())),
            workspace_id: WorkspaceSessionId("workspace-1".to_owned()),
            command: Some("printf ok".to_owned()),
            lifecycle_state: "running".to_owned(),
            finalization_state: "not_started".to_owned(),
            workspace_ownership: "existing_session".to_owned(),
            started_at_unix_ms: None,
            wall_time_ms: Some(10.0),
            transcript_path: Some(PathBuf::from("/tmp/transcript.log")),
            process_group_id: Some(1234),
        }],
        partial_errors: Vec::new(),
    }
}

fn workspace_snapshot(workspace_id: &str, upperdir: Option<PathBuf>) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(workspace_id.to_owned()),
        remount_state: "active".to_owned(),
        profile: WorkspaceProfile::HostCompatible,
        workspace_root: PathBuf::from("/workspace").join(workspace_id),
        upperdir,
        workdir: Some(PathBuf::from("/workspace").join(workspace_id).join("work")),
        namespace_fd_count: Some(3),
        base_manifest_version: Some(1),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(1),
    }
}

fn store_for_config(config: &ServerConfig) -> TestResult<ObservabilityStore> {
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    Ok(ObservabilityStore::open(&paths)?)
}

fn latest_workspace_sample(
    store: &ObservabilityStore,
    sandbox_id: &str,
    workspace_id: &str,
) -> TestResult<sandbox_observability::ResourceSampleRecord> {
    store
        .resource_samples_for_test(sandbox_id)?
        .into_iter()
        .rfind(|sample| sample.workspace_id.as_deref() == Some(workspace_id))
        .ok_or_else(|| format!("missing resource sample for {workspace_id}").into())
}

fn server_config(root: &Path, sandbox_id: Option<&str>) -> ServerConfig {
    ServerConfig {
        socket_path: root.join("runtime.sock"),
        pid_path: root.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        auth_token: None,
        sandbox_id: sandbox_id.map(str::to_owned),
    }
}

fn runtime_operations(root: &Path) -> TestResult<sandbox_runtime::SandboxRuntimeOperations> {
    let layer_stack_root = root.join("layer-stack");
    let workspace_root = root.join("runtime-workspace");
    std::fs::create_dir_all(&workspace_root)?;
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)?;
    Ok(sandbox_runtime::SandboxRuntimeOperations::from_config(
        sandbox_runtime::SandboxRuntimeConfig {
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
        },
    ))
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
