// Daemon observability: `collect`/`emit_resource_samples` write `obs.sample`
// lines per scope; the `snapshot`/`cgroup` views render from live runtime state
// plus the leaf `Reader` with no storage engine; rotation moves the oversized
// log aside; the disabled gate and missing sandbox id stay silent.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::observability::DaemonObservability;
use crate::rpc::{SandboxDaemonServer, ServerConfig};
use sandbox_config::configs::observability::ObservabilityConfig;
use sandbox_observability::{ObservabilityPaths, Reader, SampleDelta};
use sandbox_runtime::workspace_session::FinalizePolicy;
use sandbox_runtime::{
    NamespaceExecutionId, NetworkProfile, RuntimeNamespaceExecutionSnapshot,
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, WorkspaceSessionId,
};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const BIG_WINDOW_MS: i64 = 600_000;

#[test]
fn emit_resource_samples_writes_sandbox_and_workspace_scopes() -> TestResult {
    let root = test_root("emit-scopes");
    let sandbox_cgroup = root.join("cgroup-root");
    write_cgroup_fixture(&sandbox_cgroup, 4_096, 8_192, "max")?;
    let workspace_cgroup = sandbox_cgroup.join("workspace-workspace-1");
    write_cgroup_fixture(&workspace_cgroup, 2_048, 4_096, "16384")?;
    let upperdir = root.join("upperdir");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::write(upperdir.join("one.txt"), b"hello")?;

    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(sandbox_cgroup);
    let observability =
        DaemonObservability::from_config(&config).expect("sandbox id enables observability");
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: vec![RuntimeWorkspaceSnapshot {
            cgroup_path: Some(workspace_cgroup),
            ..workspace_snapshot("workspace-1", Some(upperdir))
        }],
        active_namespace_executions: Vec::new(),
        partial_errors: Vec::new(),
    };

    observability.emit_resource_samples(&config, &snapshot);

    let sandbox = latest_sample(&config, "sandbox").expect("sandbox sample written");
    assert_eq!(sandbox.metrics["cpu_usec"], 4_096);
    assert_eq!(sandbox.metrics["mem_cur"], 8_192);

    let workspace = latest_sample(&config, "workspace-1").expect("workspace sample written");
    assert_eq!(workspace.metrics["cpu_usec"], 2_048);
    assert_eq!(workspace.metrics["disk_bytes"], 5);
    assert_eq!(workspace.metrics["files"], 1);
    Ok(())
}

#[test]
fn collect_writes_sandbox_and_stack_samples_from_live_runtime() -> TestResult {
    let root = test_root("collect-stack");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let observability = server.observability.as_ref().expect("observability");

    observability.collect(&server.config, server.operations.as_ref());

    assert!(latest_sample(&server.config, "sandbox").is_some(), "sandbox sample");
    let stack = latest_sample(&server.config, "stack").expect("stack sample written");
    assert!(stack.metrics.contains_key("layer_count"));
    assert!(stack.metrics.contains_key("layers_bytes"));
    assert!(stack.metrics.contains_key("active_leases"));
    Ok(())
}

#[test]
fn cgroup_series_computes_read_time_counter_deltas() -> TestResult {
    let root = test_root("cgroup-deltas");
    let sandbox_cgroup = root.join("cgroup-root");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(sandbox_cgroup.clone());
    let observability = DaemonObservability::from_config(&config).expect("observability");

    write_cgroup_fixture(&sandbox_cgroup, 1_000, 2_000, "max")?;
    observability.emit_resource_samples(&config, &empty_snapshot());
    write_cgroup_fixture(&sandbox_cgroup, 1_600, 2_000, "max")?;
    observability.emit_resource_samples(&config, &empty_snapshot());

    let series = observability.cgroup_series("sandbox", BIG_WINDOW_MS as u64);
    let entries = series.as_array().expect("series array");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[1]["deltas"]["cpu_usec"], 600,
        "cpu_usec is delta'd at read time"
    );
    assert!(
        entries[1]["deltas"].get("mem_cur").is_none(),
        "mem_cur is a gauge, no delta"
    );
    Ok(())
}

#[test]
fn snapshot_value_renders_live_workspaces_and_latest_resources() -> TestResult {
    let root = test_root("snapshot-value");
    let upperdir = root.join("upperdir");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::write(upperdir.join("a.txt"), b"abc")?;
    let config = server_config(&root, Some("sandbox-1"));
    let observability = DaemonObservability::from_config(&config).expect("observability");
    let snapshot = RuntimeObservabilitySnapshot {
        workspaces: vec![workspace_snapshot("workspace-1", Some(upperdir))],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
        }],
        partial_errors: vec!["partial workspace projection failed".to_owned()],
    };

    observability.emit_resource_samples(&config, &snapshot);
    let value = observability.snapshot_value(snapshot);

    assert_eq!(value["sandbox_id"], "sandbox-1");
    assert_eq!(value["lifecycle_state"], "ready");
    assert_eq!(value["availability"], "partial");
    assert_eq!(value["errors"][0], "partial workspace projection failed");
    assert_eq!(value["workspaces"][0]["workspace_id"], "workspace-1");
    assert_eq!(
        value["workspaces"][0]["active_namespace_executions"][0]["namespace_execution_id"],
        "namespace_execution_1"
    );
    assert_eq!(
        value["workspaces"][0]["resources"]["latest"]["metrics"]["disk_bytes"],
        3
    );
    Ok(())
}

#[test]
fn rotation_moves_oversized_log_to_rotated_sibling() -> TestResult {
    let root = test_root("rotation");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.observability = ObservabilityConfig {
        enabled: true,
        max_file_bytes: 8,
    };
    let server = daemon_server_from(&root, config.clone())?;
    let observability = server.observability.as_ref().expect("observability");

    // First tick creates the log (no prior file to rotate); second tick rotates
    // the now-oversized log aside and writes fresh.
    observability.collect(&config, server.operations.as_ref());
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    assert!(paths.log_path().exists(), "primary log created");
    assert!(!paths.rotated_log_path().exists(), "nothing rotated yet");

    observability.collect(&config, server.operations.as_ref());
    assert!(paths.rotated_log_path().exists(), "oversized log rotated to .1");
    assert!(paths.log_path().exists(), "fresh primary written after rotation");

    // The Reader spans both files.
    assert!(latest_sample(&config, "sandbox").is_some());
    Ok(())
}

#[test]
fn from_config_disabled_when_sandbox_id_is_missing() {
    let root = test_root("missing-sandbox-id");
    let config = server_config(&root, None);
    assert!(DaemonObservability::from_config(&config).is_none());
}

#[test]
fn disabled_gate_emits_no_log() -> TestResult {
    let root = test_root("disabled");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.observability = ObservabilityConfig {
        enabled: false,
        max_file_bytes: 8 * 1024 * 1024,
    };
    let observability = DaemonObservability::from_config(&config).expect("constructed");
    observability.emit_resource_samples(&config, &empty_snapshot());

    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    assert!(!paths.log_path().exists(), "disabled emits nothing");
    Ok(())
}

#[tokio::test]
async fn cgroup_view_dispatch_returns_series() -> TestResult {
    let root = test_root("cgroup-view");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    server
        .observability
        .as_ref()
        .expect("observability")
        .collect(&server.config, server.operations.as_ref());

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::rpc::dispatch::PRIVATE_OBSERVABILITY_OP,
                "req-cgroup",
                json!({ "view": "cgroup", "scope": "sandbox" }),
            )?,
            false,
        )
        .await;

    assert_eq!(response["view"], "cgroup");
    assert_eq!(response["scope"], "sandbox");
    assert!(response["series"].is_array());
    Ok(())
}

#[tokio::test]
async fn events_view_dispatch_returns_parsed_events_by_name() -> TestResult {
    let root = test_root("events-view");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    write_log_lines(
        &server.config,
        &[
            r#"{"ts":1719500000009,"kind":"event","trace":"req-7f3","parent":"d-2","name":"lease.acquired","attrs":{"revision":"r5"}}"#,
            r#"{"ts":1719500004320,"kind":"event","trace":"req-7f3","parent":"d-8","name":"lease.released","attrs":{"revision":"r5"}}"#,
        ],
    )?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::rpc::dispatch::PRIVATE_OBSERVABILITY_OP,
                "req-events",
                json!({ "view": "events", "name": "lease.released" }),
            )?,
            false,
        )
        .await;

    assert_eq!(response["view"], "events");
    let events = response["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "only the matching name is returned");
    assert_eq!(events[0]["name"], "lease.released");
    assert_eq!(events[0]["attrs"]["revision"], "r5");
    Ok(())
}

#[tokio::test]
async fn trace_view_dispatch_folds_log_into_span_forest() -> TestResult {
    let root = test_root("trace-view");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    write_log_lines(
        &server.config,
        &[
            r#"{"ts":1719500001050,"kind":"span","trace":"req-7f3","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1048.0,"status":"completed","attrs":{"finalize_policy":"publish_then_destroy","session_created":true}}"#,
            r#"{"ts":1719500001051,"kind":"span","trace":"req-7f3","span":"d-0","name":"daemon.dispatch","dur_ms":1051.0,"status":"completed","attrs":{"op":"exec_command"}}"#,
            r#"{"ts":1719500000042,"kind":"span","trace":"req-7f3","span":"d-2","parent":"d-1","name":"workspace_session.create","dur_ms":39.0,"status":"completed","attrs":{}}"#,
            r#"{"ts":1719500000009,"kind":"event","trace":"req-7f3","parent":"d-2","name":"lease.acquired","attrs":{"revision":"r5"}}"#,
        ],
    )?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::rpc::dispatch::PRIVATE_OBSERVABILITY_OP,
                "req-trace",
                json!({ "view": "trace", "trace_id": "req-7f3" }),
            )?,
            false,
        )
        .await;

    assert_eq!(response["view"], "trace");
    assert_eq!(response["trace"], "req-7f3");
    let spans = response["spans"].as_array().expect("spans array");
    assert_eq!(spans.len(), 1, "single daemon.dispatch root");
    assert_eq!(spans[0]["span"]["name"], "daemon.dispatch");
    let command = &spans[0]["children"][0];
    assert_eq!(command["span"]["name"], "command.exec");
    let create = &command["children"][0];
    assert_eq!(create["span"]["name"], "workspace_session.create");
    assert_eq!(create["events"][0]["event"]["name"], "lease.acquired");
    Ok(())
}

#[tokio::test]
async fn events_view_dispatch_last_n_keeps_newest_matched() -> TestResult {
    let root = test_root("events-last-n");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    write_log_lines(
        &server.config,
        &[
            r#"{"ts":1000,"kind":"event","trace":"req-1","parent":"d-1","name":"lease.acquired","attrs":{}}"#,
            r#"{"ts":2000,"kind":"event","trace":"req-1","parent":"d-2","name":"lease.released","attrs":{}}"#,
            r#"{"ts":3000,"kind":"event","trace":"req-2","parent":"d-3","name":"lease.acquired","attrs":{}}"#,
        ],
    )?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::rpc::dispatch::PRIVATE_OBSERVABILITY_OP,
                "req-last-n",
                json!({ "view": "events", "last_n": 2 }),
            )?,
            false,
        )
        .await;

    let events = response["events"].as_array().expect("events array");
    assert_eq!(events.len(), 2, "last_n caps the fold to the newest N");
    // The fold is oldest-first; last_n drops the oldest, keeping ts 2000 and 3000.
    assert_eq!(events[0]["ts"], 2000);
    assert_eq!(events[1]["ts"], 3000);
    Ok(())
}

#[tokio::test]
async fn trace_view_dispatch_last_resolves_most_recent_root() -> TestResult {
    let root = test_root("trace-last");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    write_log_lines(
        &server.config,
        &[
            r#"{"ts":1000,"kind":"span","trace":"req-old","span":"d-0","name":"daemon.dispatch","dur_ms":100.0,"status":"completed","attrs":{}}"#,
            r#"{"ts":2000,"kind":"span","trace":"req-new","span":"d-1","name":"daemon.dispatch","dur_ms":100.0,"status":"completed","attrs":{}}"#,
        ],
    )?;

    let response = server
        .dispatch_bytes(
            request_bytes(
                crate::rpc::dispatch::PRIVATE_OBSERVABILITY_OP,
                "req-trace-last",
                json!({ "view": "trace", "trace_id": "last" }),
            )?,
            false,
        )
        .await;

    // "last" resolves to the root span with the latest start, not a trace named "last".
    assert_eq!(response["trace"], "req-new");
    let spans = response["spans"].as_array().expect("spans array");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["span"]["trace"], "req-new");
    Ok(())
}

#[tokio::test]
async fn observability_emit_does_not_change_operation_responses() -> TestResult {
    let root = test_root("emit-isolated");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(request_bytes("unknown_runtime_op", "req-1", json!({}))?, false)
        .await;

    assert_eq!(
        response,
        sandbox_protocol::Response::unknown_op().into_json_value()
    );
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
    std::fs::write(dir.join("memory.current"), format!("{memory_current_bytes}\n"))?;
    std::fs::write(dir.join("memory.max"), format!("{memory_max}\n"))?;
    Ok(())
}

fn latest_sample(config: &ServerConfig, scope: &str) -> Option<SampleDelta> {
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path).expect("paths");
    Reader::new(
        paths.log_path().to_path_buf(),
        paths.rotated_log_path().to_path_buf(),
    )
    .samples(scope, BIG_WINDOW_MS)
    .pop()
}

fn write_log_lines(config: &ServerConfig, lines: &[&str]) -> TestResult {
    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    if let Some(parent) = paths.log_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(paths.log_path(), format!("{}\n", lines.join("\n")))?;
    Ok(())
}

fn empty_snapshot() -> RuntimeObservabilitySnapshot {
    RuntimeObservabilitySnapshot::default()
}

fn workspace_snapshot(workspace_id: &str, upperdir: Option<PathBuf>) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(workspace_id.to_owned()),
        network: NetworkProfile::Shared,
        finalize_policy: FinalizePolicy::NoOp,
        workspace_root: PathBuf::from("/workspace").join(workspace_id),
        upperdir,
        workdir: Some(PathBuf::from("/workspace").join(workspace_id).join("work")),
        namespace_fd_count: Some(3),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(1),
        layer_ids: vec![format!("{workspace_id}-layer")],
        cgroup_path: None,
    }
}

fn daemon_server(root: &Path, sandbox_id: Option<&str>) -> TestResult<SandboxDaemonServer> {
    daemon_server_from(root, server_config(root, sandbox_id))
}

fn daemon_server_from(root: &Path, config: ServerConfig) -> TestResult<SandboxDaemonServer> {
    Ok(SandboxDaemonServer::new_with_runtime_config(
        config,
        runtime_config(root)?,
    ))
}

fn request_bytes(op: &str, request_id: &str, args: Value) -> TestResult<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "op": op,
        "request_id": request_id,
        "scope": { "kind": "sandbox", "sandbox_id": "sandbox-1" },
        "args": args,
    }))?)
}

fn server_config(root: &Path, sandbox_id: Option<&str>) -> ServerConfig {
    ServerConfig {
        socket_path: root.join("runtime.sock"),
        pid_path: root.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        http_host: None,
        http_port: None,
        auth_token: None,
        sandbox_id: sandbox_id.map(str::to_owned),
        cgroup_root: None,
        observability: ObservabilityConfig::default(),
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
                setup_timeout_s: 30.0,
                exit_grace_s: 0.25,
                rfc1918_egress: sandbox_runtime::Rfc1918Egress::Allow,
            },
        },
        namespace_execution: sandbox_runtime::NamespaceExecutionRuntimeConfig {
            scratch_root: root.join("command-scratch"),
        },
        layerstack: sandbox_runtime::LayerstackRuntimeConfig::default(),
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
