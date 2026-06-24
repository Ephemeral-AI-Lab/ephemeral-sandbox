use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::observability::DaemonObservability;
use crate::server::{SandboxDaemonServer, ServerConfig};
use sandbox_observability::{ObservabilityPaths, ObservabilityStore, SpanRecord, TraceRecord};
use sandbox_runtime::command::CommandSessionId;
use sandbox_runtime::{
    span_keys, BeginNamespaceExecution, CommandFinalizationTraceMetadata,
    CompleteNamespaceExecution, CompletedOperationSpan, CompletedOperationTrace,
    NamespaceExecutionId, NamespaceExecutionLifecycle, NamespaceExecutionRecord,
    NamespaceExecutionTerminalStatus, SandboxRuntimeOperations, WorkspaceSessionId,
};
use sandbox_runtime::{
    RuntimeNamespaceExecutionSnapshot, RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot,
    WorkspaceProfile,
};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[test]
fn observability_collection_writes_namespace_only_live_snapshot() -> TestResult {
    let root = test_root("collects-namespace-only");
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
fn observability_collection_writes_namespace_execution_tables() -> TestResult {
    let root = test_root("namespace-projection");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: Vec::new(),
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
            lifecycle_state: NamespaceExecutionLifecycle::Running,
            started_at_unix_ms: 1_000,
        }],
        completed_namespace_executions: vec![completed_namespace_execution(
            "namespace_execution_2",
            "workspace-1",
            "exec_command",
            NamespaceExecutionTerminalStatus::Ok,
            Some("req-parent"),
            Some(0),
        )],
        partial_errors: Vec::new(),
    };

    observability.collect_runtime_snapshot_for_test(&config, snapshot)?;

    let store = store_for_config(&config)?;
    let snapshots = store.namespace_execution_snapshots_for_test("sandbox-1")?;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].namespace_execution_id, "namespace_execution_1");
    assert_eq!(snapshots[0].workspace_session_id, "workspace-1");
    assert_eq!(snapshots[0].operation, "exec_command");
    assert_eq!(snapshots[0].lifecycle_state, "running");

    let traces = store.namespace_execution_traces_for_test("sandbox-1")?;
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].trace_id, "namespace_execution:namespace_execution_2");
    assert_eq!(traces[0].namespace_execution_id, "namespace_execution_2");
    assert_eq!(traces[0].workspace_session_id, "workspace-1");
    assert_eq!(traces[0].request_id.as_deref(), Some("req-parent"));
    assert_eq!(traces[0].status, "ok");
    assert_eq!(traces[0].exit_code, Some(0));
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
            lifecycle_state: NamespaceExecutionLifecycle::Running,
            started_at_unix_ms: 1_000,
        }],
        completed_namespace_executions: Vec::new(),
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
fn daemon_collect_acks_only_successful_namespace_trace_projection() -> TestResult {
    let root = test_root("namespace-ack-success-only");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let good = seed_completed_namespace_execution(
        server.operations.as_ref(),
        "namespace_execution_good",
        "exec_command",
    );
    let bad = seed_completed_namespace_execution(
        server.operations.as_ref(),
        "namespace_execution_bad",
        "",
    );

    server
        .observability
        .as_ref()
        .expect("sandbox id enables observability")
        .collect(&server.config, server.operations.as_ref())?;

    let pending = server
        .operations
        .drain_completed_namespace_executions_for_test(10)
        .expect("pending namespace records drain");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].namespace_execution_id, bad);

    let store = store_for_config(&server.config)?;
    let traces = store.namespace_execution_traces_for_test("sandbox-1")?;
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].namespace_execution_id, good.0);
    let sandbox = store
        .sandbox_snapshot_for_test("sandbox-1")?
        .expect("sandbox snapshot written");
    assert!(sandbox
        .error_message
        .as_deref()
        .is_some_and(|message| message.contains("namespace_execution_bad")));
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
            lifecycle_state: NamespaceExecutionLifecycle::Running,
            started_at_unix_ms: 1_000,
        }],
        completed_namespace_executions: Vec::new(),
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
            completed_namespace_executions: Vec::new(),
            partial_errors: Vec::new(),
        },
    )?;
    std::fs::write(upperdir.join("two.txt"), b"2")?;
    observability.collect_runtime_snapshot_for_test(
        &config,
        RuntimeObservabilitySnapshot {
            workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir.clone()))],
            active_namespace_executions: Vec::new(),
            completed_namespace_executions: Vec::new(),
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
            completed_namespace_executions: Vec::new(),
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
            completed_namespace_executions: Vec::new(),
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
    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-summary".to_owned(),
        "exec_command".to_owned(),
        &json!({
            "status": "completed",
            "output": "SECRET_OUTPUT",
            "transcript": "SECRET_TRANSCRIPT",
        }),
        completed_trace(&[
            (None, "dispatch_operation", 0),
            (Some(0), "SECRET_SPAN_METHOD", 1),
        ]),
    )?;
    observability.insert_completed_async_operation_trace(
        completed_trace(&[(None, "complete_terminal_command_with_services", 0)]),
        command_finalization_metadata("req-origin", "workspace-1", "SECRET_COMMAND_SESSION"),
    )?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::server::dispatch::PRIVATE_OBSERVABILITY_SNAPSHOT_OP,
                "req-private-snapshot",
                json!({
                    "include_recent_traces": true,
                    "trace_limit": 20,
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
    assert!(response["recent_traces"]
        .as_array()
        .expect("recent traces")
        .iter()
        .any(|trace| trace["trace_id"] == "request:req-summary"));
    let response_text = response.to_string();
    for forbidden in [
        "SECRET_OUTPUT",
        "SECRET_TRANSCRIPT",
        "SECRET_SPAN_METHOD",
        "SECRET_COMMAND_SESSION",
        "span_id",
        "method_name",
        "command_session_id",
    ] {
        assert!(
            !response_text.contains(forbidden),
            "private snapshot response unexpectedly contained {forbidden}: {response_text}"
        );
    }
    Ok(())
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
fn completed_async_command_finalization_trace_maps_and_persists_success() -> TestResult {
    let root = test_root("async-trace-success");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_async_operation_trace(
        completed_trace(&[
            (None, "complete_terminal_command_with_services", 0),
            (Some(0), "apply_workspace_completion_policy", 1),
            (Some(0), "complete_command_record", 2),
        ]),
        command_finalization_metadata("req-origin", "workspace-1", "cmd_1"),
    )?;

    let store = store_for_config(&config)?;
    let trace_id = "async:command_finalization:command_session_id:cmd_1";
    let trace = trace_for(&store, trace_id)?;
    assert_eq!(trace.kind, "async");
    assert_eq!(trace.status, "ok");
    assert_eq!(trace.sandbox_id, "sandbox-1");
    assert_eq!(trace.operation, "command_finalization");
    assert!(trace.request_id.is_none());
    assert_eq!(trace.origin_request_id.as_deref(), Some("req-origin"));
    assert_eq!(trace.workspace_id.as_deref(), Some("workspace-1"));
    assert_eq!(trace.command_session_id.as_deref(), Some("cmd_1"));

    let spans = store.spans_for_test(trace_id)?;
    assert_eq!(
        span_names(&spans),
        vec![
            "complete_terminal_command_with_services",
            "apply_workspace_completion_policy",
            "complete_command_record",
        ]
    );
    assert_eq!(
        spans[1].parent_span_id.as_deref(),
        Some("async:command_finalization:command_session_id:cmd_1:span:0")
    );
    assert_eq!(
        spans[2].parent_span_id.as_deref(),
        Some("async:command_finalization:command_session_id:cmd_1:span:0")
    );
    Ok(())
}

#[test]
fn completed_async_command_finalization_trace_maps_error_text() -> TestResult {
    let root = test_root("async-trace-error");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_async_operation_trace(
        completed_trace(&[(None, "complete_terminal_command_with_services", 0)]),
        CommandFinalizationTraceMetadata {
            origin_request_id: "req-origin".to_owned(),
            workspace_session_id: Some(WorkspaceSessionId("workspace-1".to_owned())),
            command_session_id: CommandSessionId("cmd_1".to_owned()),
            finalizer_error: Some("raw finalizer error".to_owned()),
        },
    )?;

    let store = store_for_config(&config)?;
    let trace = trace_for(&store, "async:command_finalization:command_session_id:cmd_1")?;
    assert_eq!(trace.status, "error");
    assert!(trace.error_kind.is_none());
    assert_eq!(trace.error_message.as_deref(), Some("raw finalizer error"));
    Ok(())
}

#[test]
fn async_command_finalization_trace_does_not_update_deep_span_keys() -> TestResult {
    let root = test_root("async-trace-no-deep-keys");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_async_operation_trace(
        completed_trace_with_durations(&[
            (None, "complete_terminal_command_with_services", 0, 20.0),
            (Some(0), "CommandOperationService::exec_command", 1, 101.0),
        ]),
        command_finalization_metadata("req-origin", "workspace-1", "cmd_1"),
    )?;

    assert!(observability.enabled_deep_span_keys().is_empty());
    Ok(())
}

#[test]
fn async_trace_sink_store_failure_is_swallowed_at_callback_boundary() -> TestResult {
    let root = test_root("async-trace-store-failure");
    let config = server_config(&root, Some("sandbox-1"));
    let observability = Arc::new(
        DaemonObservability::from_config(&config).expect("sandbox id enables observability"),
    );
    observability.force_sqlite_write_errors_for_test()?;
    let sink = DaemonObservability::async_trace_sink(Arc::clone(&observability));

    sink(
        completed_trace(&[(None, "complete_terminal_command_with_services", 0)]),
        command_finalization_metadata("req-origin", "workspace-1", "cmd_1"),
    );

    assert!(observability.enabled_deep_span_keys().is_empty());
    Ok(())
}

#[test]
fn completed_operation_trace_marks_phase3_service_span_on_error() -> TestResult {
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
            (Some(2), "command.exec.workspace.resolve", 3),
            (Some(2), "command.exec.process.start", 4),
        ]),
    )?;

    let store = store_for_config(&config)?;
    let trace = trace_for(&store, "request:req-error")?;
    assert_eq!(trace.status, "error");
    assert_eq!(trace.error_kind.as_deref(), Some("operation_failed"));
    assert_eq!(trace.error_message.as_deref(), Some("command failed"));
    let spans = store.spans_for_test("request:req-error")?;
    let service_span = span_record(&spans, "CommandOperationService::exec_command");
    assert_eq!(service_span.status, "error");
    assert_eq!(service_span.error_kind.as_deref(), Some("operation_failed"));
    assert_eq!(service_span.error_message.as_deref(), Some("command failed"));
    for child_name in [
        "command.exec.workspace.resolve",
        "command.exec.process.start",
    ] {
        let child_span = span_record(&spans, child_name);
        assert_eq!(child_span.status, "ok");
        assert!(child_span.error_kind.is_none());
    }
    assert_no_trace_text(&trace, &spans, "SECRET_OUTPUT");
    Ok(())
}

#[test]
fn fast_parent_spans_do_not_enable_deep_span_keys() -> TestResult {
    let root = test_root("trace-fast-parents");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-fast-command".to_owned(),
        "exec_command".to_owned(),
        &json!({ "status": "ok" }),
        completed_trace_with_durations(&[
            (None, "dispatch_operation", 0, 99.0),
            (Some(0), "exec_command::dispatch", 1, 99.0),
            (Some(1), "CommandOperationService::exec_command", 2, 99.0),
        ]),
    )?;
    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-fast-squash".to_owned(),
        "squash".to_owned(),
        &json!({ "squashed": false }),
        completed_trace_with_durations(&[
            (None, "dispatch_operation", 0, 99.0),
            (Some(0), "squash::dispatch", 1, 99.0),
            (Some(1), "LayerStackService::squash", 2, 99.0),
        ]),
    )?;

    assert!(observability.enabled_deep_span_keys().is_empty());
    Ok(())
}

#[test]
fn slow_command_parent_enables_command_deep_span_keys() -> TestResult {
    let root = test_root("trace-enable-command-keys");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    assert!(observability.enabled_deep_span_keys().is_empty());
    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-slow-command".to_owned(),
        "exec_command".to_owned(),
        &json!({ "status": "ok" }),
        completed_trace_with_durations(&[
            (None, "dispatch_operation", 0, 20.0),
            (Some(0), "exec_command::dispatch", 1, 20.0),
            (Some(1), "CommandOperationService::exec_command", 2, 101.0),
        ]),
    )?;

    assert_eq!(
        span_key_names(observability.enabled_deep_span_keys()),
        vec![
            span_keys::COMMAND_EXEC_PROCESS_START.as_str(),
            span_keys::COMMAND_EXEC_WORKSPACE_CREATE_ONE_SHOT_SESSION.as_str(),
            span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE.as_str(),
            span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE_EXISTING_SESSION.as_str(),
        ]
    );
    Ok(())
}

#[test]
fn slow_layerstack_parent_enables_layerstack_deep_span_keys() -> TestResult {
    let root = test_root("trace-enable-layerstack-keys");
    let config = server_config(&root, Some("sandbox-1"));
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-slow-squash".to_owned(),
        "squash".to_owned(),
        &json!({ "squashed": false }),
        completed_trace_with_durations(&[
            (None, "dispatch_operation", 0, 20.0),
            (Some(0), "squash::dispatch", 1, 20.0),
            (Some(1), "LayerStackService::squash", 2, 101.0),
        ]),
    )?;

    assert_eq!(
        span_key_names(observability.enabled_deep_span_keys()),
        vec![
            span_keys::LAYERSTACK_SQUASH_COMPACT_STACK.as_str(),
            span_keys::LAYERSTACK_SQUASH_OPEN_STACK.as_str(),
        ]
    );
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
async fn learned_deep_span_keys_apply_to_next_dispatched_request() -> TestResult {
    let root = test_root("trace-learned-keys-bridge");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let observability = server
        .observability
        .as_ref()
        .expect("sandbox id enables observability");

    let initial_response = server
        .dispatch_bytes(request_bytes("squash", "req-unlearned-squash", json!({}))?, false)
        .await;

    assert_eq!(initial_response["squashed"], false);
    let store = store_for_config(&server.config)?;
    assert_eq!(
        span_names(&store.spans_for_test("request:req-unlearned-squash")?),
        vec![
            "dispatch_operation",
            "squash::dispatch",
            "LayerStackService::squash",
        ]
    );

    observability.insert_completed_operation_trace(
        "sandbox-1".to_owned(),
        "req-prime-squash".to_owned(),
        "squash".to_owned(),
        &json!({ "squashed": false }),
        completed_trace_with_durations(&[
            (None, "dispatch_operation", 0, 20.0),
            (Some(0), "squash::dispatch", 1, 20.0),
            (Some(1), "LayerStackService::squash", 2, 101.0),
        ]),
    )?;

    let response = server
        .dispatch_bytes(request_bytes("squash", "req-learned-squash", json!({}))?, false)
        .await;

    assert_eq!(response["squashed"], false);
    let spans = store.spans_for_test("request:req-learned-squash")?;
    assert_eq!(
        span_names(&spans),
        vec![
            "dispatch_operation",
            "squash::dispatch",
            "LayerStackService::squash",
            "layerstack.squash.open_stack",
            "layerstack.squash.compact_stack",
        ]
    );
    assert_eq!(
        spans[3].parent_span_id.as_deref(),
        Some("request:req-learned-squash:span:2")
    );
    assert_eq!(
        spans[4].parent_span_id.as_deref(),
        Some("request:req-learned-squash:span:2")
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
    let server = daemon_server(&root, Some("sandbox-1"))?;
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
    completed_trace_with_durations(
        &spans
            .iter()
            .map(|(parent_call_index, method_name, call_index)| {
                (*parent_call_index, *method_name, *call_index, 10.0)
            })
            .collect::<Vec<_>>(),
    )
}

fn completed_trace_with_durations(
    spans: &[(Option<i64>, &'static str, i64, f64)],
) -> CompletedOperationTrace {
    CompletedOperationTrace {
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: 1_050,
        duration_ms: 50.0,
        spans: spans
            .iter()
            .map(
                |(parent_call_index, method_name, call_index, duration_ms)| {
                    CompletedOperationSpan {
                parent_call_index: *parent_call_index,
                method_name,
                call_index: *call_index,
                status: "ok",
                started_at_unix_ms: 1_000 + *call_index,
                finished_at_unix_ms: 1_010 + *call_index,
                        duration_ms: *duration_ms,
                    }
                },
            )
            .collect(),
    }
}

fn command_finalization_metadata(
    origin_request_id: &str,
    workspace_session_id: &str,
    command_session_id: &str,
) -> CommandFinalizationTraceMetadata {
    CommandFinalizationTraceMetadata {
        origin_request_id: origin_request_id.to_owned(),
        workspace_session_id: Some(WorkspaceSessionId(workspace_session_id.to_owned())),
        command_session_id: CommandSessionId(command_session_id.to_owned()),
        finalizer_error: None,
    }
}

fn completed_namespace_execution(
    namespace_execution_id: &str,
    workspace_session_id: &str,
    operation_name: &str,
    terminal_status: NamespaceExecutionTerminalStatus,
    request_id: Option<&str>,
    exit_code: Option<i64>,
) -> NamespaceExecutionRecord {
    NamespaceExecutionRecord {
        namespace_execution_id: NamespaceExecutionId(namespace_execution_id.to_owned()),
        workspace_session_id: WorkspaceSessionId(workspace_session_id.to_owned()),
        operation_name: operation_name.to_owned(),
        request_id: request_id.map(str::to_owned),
        lifecycle_state: NamespaceExecutionLifecycle::Terminal,
        started_at_unix_ms: 1_000,
        finished_at_unix_ms: Some(1_025),
        duration_ms: Some(25.0),
        terminal_status: Some(terminal_status),
        exit_code,
        error_kind: None,
        error_message: None,
    }
}

fn seed_completed_namespace_execution(
    operations: &SandboxRuntimeOperations,
    namespace_execution_id: &str,
    operation_name: &str,
) -> NamespaceExecutionId {
    let id = NamespaceExecutionId(namespace_execution_id.to_owned());
    operations
        .begin_namespace_execution_for_test(
            id.clone(),
            BeginNamespaceExecution {
                workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
                operation_name: operation_name.to_owned(),
                request_id: Some("req-parent".to_owned()),
            },
        )
        .expect("begin namespace execution succeeds");
    operations
        .complete_namespace_execution_for_test(
            &id,
            CompleteNamespaceExecution {
                terminal_status: NamespaceExecutionTerminalStatus::Ok,
                exit_code: Some(0),
                error_kind: None,
                error_message: None,
            },
        )
        .expect("complete namespace execution succeeds");
    id
}

fn span_key_names(keys: Vec<sandbox_runtime::SpanKey>) -> Vec<&'static str> {
    let mut names = keys.into_iter().map(|key| key.as_str()).collect::<Vec<_>>();
    names.sort_unstable();
    names
}

fn span_record<'a>(spans: &'a [SpanRecord], method_name: &str) -> &'a SpanRecord {
    spans
        .iter()
        .find(|span| span.method_name == method_name)
        .expect("span recorded")
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
    values.extend(trace.origin_request_id.as_deref());
    values.extend(trace.workspace_id.as_deref());
    values.extend(trace.command_session_id.as_deref());
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
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
            lifecycle_state: NamespaceExecutionLifecycle::Running,
            started_at_unix_ms: 1_000,
        }],
        completed_namespace_executions: Vec::new(),
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

fn runtime_config(root: &Path) -> TestResult<sandbox_runtime::SandboxRuntimeConfig> {
    let layer_stack_root = root.join("layer-stack");
    let workspace_root = root.join("runtime-workspace");
    std::fs::create_dir_all(&workspace_root)?;
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)?;
    Ok(sandbox_runtime::SandboxRuntimeConfig {
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
