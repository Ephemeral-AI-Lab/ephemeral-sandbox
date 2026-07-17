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
use sandbox_observability_telemetry::{LayerStackBytes, ObservabilityPaths, Reader, SampleDelta};
use sandbox_operation_catalog::observability::{
    CGROUP_SPEC, EVENTS_SPEC, LAYERSTACK_SPEC, SNAPSHOT_SPEC, TRACE_SPEC,
};
use sandbox_runtime::workspace_session::FinalizePolicy;
use sandbox_runtime::{
    NamespaceExecutionId, NetworkProfile, RuntimeNamespaceExecutionSnapshot,
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, WorkspaceSessionId,
};
use sandbox_runtime_layerstack::{LayerChange, LayerPath, LayerStack};
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
    #[cfg(unix)]
    assert!(workspace.metrics["disk_allocated_bytes"].is_u64());
    #[cfg(not(unix))]
    assert!(workspace.metrics.get("disk_allocated_bytes").is_none());
    assert_eq!(workspace.metrics["files"], 1);
    Ok(())
}

#[test]
fn collect_writes_sandbox_and_stack_samples_from_live_runtime() -> TestResult {
    let root = test_root("collect-stack");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let observability = server.observability.as_ref().expect("observability");

    observability.collect(&server.config, server.operations.as_ref());

    assert!(
        latest_sample(&server.config, "sandbox").is_some(),
        "sandbox sample"
    );
    let stack = latest_sample(&server.config, "stack").expect("stack sample written");
    assert!(stack.metrics["layer_count"].is_u64());
    assert!(stack.metrics["layers_bytes"].is_u64());
    assert!(stack.metrics["storage_logical_bytes"].is_u64());
    #[cfg(unix)]
    {
        assert!(stack.metrics["layers_allocated_bytes"].is_u64());
        assert!(stack.metrics["storage_allocated_bytes"].is_u64());
    }
    #[cfg(not(unix))]
    {
        assert!(stack.metrics["layers_allocated_bytes"].is_null());
        assert!(stack.metrics["storage_allocated_bytes"].is_null());
    }
    assert_eq!(stack.metrics["staging_entry_count"], 0);
    assert_eq!(stack.metrics["active_leases"], 0);
    Ok(())
}

#[test]
fn collect_preserves_unavailable_stack_metrics_as_null() -> TestResult {
    let root = test_root("collect-stack-unavailable");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.observability.sampling.max_walk_nodes = 1;
    let server = daemon_server_from(&root, config)?;
    let layer_stack_root = server.operations.layer_stack_root();
    let mut layerstack = LayerStack::open(layer_stack_root.to_path_buf())?;
    layerstack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse("nonempty.txt")?,
        content: b"force a truncated active-layer walk".to_vec(),
    }])?;
    std::fs::remove_dir_all(layer_stack_root.join(".layer-metadata"))?;
    std::fs::remove_dir_all(layer_stack_root.join("staging"))?;

    server
        .observability
        .as_ref()
        .expect("observability")
        .collect(&server.config, server.operations.as_ref());

    let stack = latest_sample(&server.config, "stack").expect("stack sample written");
    assert!(stack.metrics["layer_count"].is_u64());
    for metric in [
        "layers_bytes",
        "layers_allocated_bytes",
        "storage_logical_bytes",
        "storage_allocated_bytes",
    ] {
        assert!(
            stack.metrics[metric].is_null(),
            "unavailable {metric} must be explicit null, never omitted or zero"
        );
    }
    assert_eq!(
        stack.metrics["staging_entry_count"], 0,
        "opening the live stack repairs a missing staging directory before sampling"
    );
    Ok(())
}

#[test]
fn stack_metrics_serialize_unavailable_values_as_explicit_nulls() {
    let metrics = DaemonObservability::stack_metrics(2, 1, &LayerStackBytes::default());

    assert_eq!(metrics["layer_count"], 2);
    assert_eq!(metrics["active_leases"], 1);
    for metric in [
        "layers_bytes",
        "layers_allocated_bytes",
        "storage_logical_bytes",
        "storage_allocated_bytes",
        "staging_entry_count",
    ] {
        assert!(
            metrics[metric].is_null(),
            "unavailable {metric} must be explicit null, never omitted or zero"
        );
    }
}

#[test]
fn adapter_maps_concrete_runtime_snapshot_into_neutral_input() {
    let snapshot = crate::observability::adapter::map_snapshot(RuntimeObservabilitySnapshot {
        workspaces: vec![workspace_snapshot("workspace-1", None)],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
        }],
        partial_errors: vec!["partial projection".to_owned()],
    });

    assert_eq!(snapshot.partial_errors, ["partial projection"]);
    assert_eq!(snapshot.workspaces[0].workspace_id, "workspace-1");
    assert_eq!(snapshot.workspaces[0].network_profile, "shared");
    assert_eq!(snapshot.workspaces[0].finalize_policy, "no_op");
    assert_eq!(snapshot.workspaces[0].namespace_fd_count, Some(3));
    assert_eq!(
        snapshot.workspaces[0].base_root_hash.as_deref(),
        Some("root")
    );
    assert_eq!(snapshot.workspaces[0].layer_count, Some(1));
    assert_eq!(
        snapshot.active_namespace_executions[0].namespace_execution_id,
        "namespace_execution_1"
    );
    assert_eq!(
        snapshot.active_namespace_executions[0].workspace_session_id,
        "workspace-1"
    );
    assert_eq!(
        snapshot.active_namespace_executions[0].operation_name,
        "exec_command"
    );
}

#[test]
fn rotation_moves_oversized_log_to_rotated_sibling() -> TestResult {
    let root = test_root("rotation");
    let mut config = server_config(&root, Some("sandbox-1"));
    config.observability = ObservabilityConfig {
        enabled: true,
        max_file_bytes: 8,
        ..ObservabilityConfig::default()
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
    assert!(
        paths.rotated_log_path().exists(),
        "oversized log rotated to .1"
    );
    assert!(
        paths.log_path().exists(),
        "fresh primary written after rotation"
    );

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
        ..ObservabilityConfig::default()
    };
    let observability = DaemonObservability::from_config(&config).expect("constructed");
    observability.emit_resource_samples(&config, &empty_snapshot());

    let paths = ObservabilityPaths::from_socket_path(&config.socket_path)?;
    assert!(!paths.log_path().exists(), "disabled emits nothing");
    Ok(())
}

#[tokio::test]
async fn concrete_cgroup_operation_returns_series() -> TestResult {
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
                CGROUP_SPEC.name,
                "req-cgroup",
                json!({ "scope": "sandbox" }),
            )?,
            false,
        )
        .await;
    let response = response.as_json_value();

    assert_eq!(response["view"], "cgroup");
    assert_eq!(response["scope"], "sandbox");
    assert_eq!(response["topology"]["available"], false);
    assert_eq!(response["topology"]["error"], "cgroup root unavailable");
    assert_eq!(response["series"].as_array().map(Vec::len), Some(1));
    assert_eq!(response["series"][0]["metrics"]["cgroup_available"], false);
    assert_eq!(
        response["series"][0]["metrics"]["cgroup_error"],
        "cgroup root unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_refreshes_resources_before_building_response() -> TestResult {
    let root = test_root("snapshot-refresh");
    let cgroup_root = root.join("cgroup-root");
    write_cgroup_fixture(&cgroup_root, 1_024, 2_048, "4096")?;
    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(cgroup_root.clone());
    let server = daemon_server_from(&root, config)?;
    let observability = server.observability.as_ref().expect("observability");

    observability.emit_resource_samples(&server.config, &empty_snapshot());
    assert_eq!(
        latest_sample(&server.config, "sandbox")
            .expect("stale baseline")
            .metrics["cpu_usec"],
        1_024
    );

    write_cgroup_fixture(&cgroup_root, 8_192, 16_384, "32768")?;
    let response = server
        .dispatch_bytes(
            request_bytes(SNAPSHOT_SPEC.name, "req-refresh", json!({}))?,
            false,
        )
        .await;
    let response = response.as_json_value();

    assert_eq!(
        response["resources"]["latest"]["metrics"]["cpu_usec"],
        8_192
    );
    assert_eq!(
        response["resources"]["latest"]["metrics"]["mem_cur"],
        16_384
    );
    assert_eq!(
        response["resources"]["latest"]["metrics"]["mem_max"],
        32_768
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_refresh_failure_never_reuses_rotated_stale_sample() -> TestResult {
    let root = test_root("snapshot-refresh-failure");
    let cgroup_root = root.join("cgroup-root");
    write_cgroup_fixture(&cgroup_root, 1_024, 2_048, "4096")?;
    let mut config = server_config(&root, Some("sandbox-1"));
    config.cgroup_root = Some(cgroup_root.clone());
    let server = daemon_server_from(&root, config)?;
    let observability = server.observability.as_ref().expect("observability");

    observability.emit_resource_samples(&server.config, &empty_snapshot());
    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;
    std::fs::rename(paths.log_path(), paths.rotated_log_path())?;
    std::fs::create_dir(paths.log_path())?;
    write_cgroup_fixture(&cgroup_root, 8_192, 16_384, "32768")?;

    let response = server
        .dispatch_bytes(
            request_bytes(SNAPSHOT_SPEC.name, "req-refresh-failure", json!({}))?,
            false,
        )
        .await;
    let response = response.as_json_value();

    assert_eq!(response["error"]["kind"], "internal_error");
    assert_eq!(
        response["error"]["message"],
        "snapshot resource refresh failed"
    );
    assert!(response.get("resources").is_none());
    assert_eq!(
        latest_sample(&server.config, "sandbox")
            .expect("rotated stale sample remains readable")
            .metrics["cpu_usec"],
        1_024
    );
    Ok(())
}

#[tokio::test]
async fn concrete_observability_operations_dispatch_end_to_end() -> TestResult {
    let root = test_root("concrete-observability-operations");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let snapshot = server
        .dispatch_bytes(
            request_bytes(SNAPSHOT_SPEC.name, "req-snapshot", json!({}))?,
            false,
        )
        .await;
    let snapshot = snapshot.as_json_value();

    assert_eq!(snapshot["sandbox_id"], "sandbox-1");
    assert_eq!(snapshot["lifecycle_state"], "ready");
    assert_eq!(snapshot["availability"], "available");
    assert_eq!(snapshot["errors"], json!([]));
    assert_eq!(snapshot["resources"]["history"], json!([]));
    assert_eq!(
        snapshot["resources"]["latest"]["metrics"]["cgroup_available"],
        false
    );
    assert_eq!(
        snapshot["resources"]["latest"]["metrics"]["cgroup_error"],
        "cgroup root unavailable"
    );
    assert_eq!(snapshot["workspaces"], json!([]));
    assert!(snapshot["sampled_at_unix_ms"].is_u64());
    assert!(snapshot["daemon"]["daemon_pid"].is_u64());
    assert!(snapshot["daemon"]["runtime_dir"].is_string());
    assert!(snapshot["stack"]["layer_count"].is_u64());
    assert!(snapshot["stack"]["layers_bytes"].is_u64());
    assert_eq!(snapshot["stack"]["active_leases"], 0);

    let cgroup = server
        .dispatch_bytes(
            request_bytes(CGROUP_SPEC.name, "req-cgroup", json!({ "scope": "sandbox" }))?,
            false,
        )
        .await;
    let cgroup = cgroup.as_json_value();
    assert_eq!(cgroup["view"], "cgroup");
    assert_eq!(cgroup["topology"]["schema_version"], 2);
    assert_eq!(cgroup["topology"]["workspaces"], json!([]));

    let trace = server
        .dispatch_bytes(
            request_bytes(TRACE_SPEC.name, "req-trace", json!({ "trace_id": "last" }))?,
            false,
        )
        .await;
    let trace = trace.as_json_value();
    assert_eq!(trace["view"], "trace");
    assert_eq!(trace["trace"], "last");
    assert_eq!(trace["spans"], json!([]));

    let events = server
        .dispatch_bytes(
            request_bytes(EVENTS_SPEC.name, "req-events", json!({}))?,
            false,
        )
        .await;
    let events = events.as_json_value();
    assert_eq!(events["view"], "events");
    assert_eq!(events["events"], json!([]));

    let layerstack = server
        .dispatch_bytes(
            request_bytes(LAYERSTACK_SPEC.name, "req-layerstack", json!({}))?,
            false,
        )
        .await;
    let layerstack = layerstack.as_json_value();

    assert_eq!(layerstack["view"], "layerstack");
    assert!(layerstack["manifest_version"].is_u64());
    assert!(layerstack["root_hash"].is_string());
    assert_eq!(layerstack["active_lease_count"], 0);
    assert!(layerstack["total_bytes"].is_u64());
    assert!(layerstack["layers"].is_array());
    Ok(())
}

#[tokio::test]
async fn observability_emit_does_not_change_operation_responses() -> TestResult {
    let root = test_root("emit-isolated");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(
            request_bytes("unknown_runtime_op", "req-1", json!({}))?,
            false,
        )
        .await;

    assert_eq!(
        response,
        sandbox_operation_contract::OperationResponse::unknown_op()
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
    std::fs::write(
        dir.join("cpu.stat"),
        format!("usage_usec {cpu_usage_usec}\n"),
    )?;
    std::fs::write(
        dir.join("memory.current"),
        format!("{memory_current_bytes}\n"),
    )?;
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

fn empty_snapshot() -> RuntimeObservabilitySnapshot {
    RuntimeObservabilitySnapshot::default()
}

fn workspace_snapshot(workspace_id: &str, upperdir: Option<PathBuf>) -> RuntimeWorkspaceSnapshot {
    RuntimeWorkspaceSnapshot {
        workspace_id: WorkspaceSessionId(workspace_id.to_owned()),
        holder_pid: i32::try_from(std::process::id()).expect("test pid fits i32"),
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
        limits: sandbox_protocol::ProtocolLimits::default(),
        max_concurrent_connections: 256,
        forward: Default::default(),
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
                freeze_budget_s: 0.5,
            },
        },
        namespace_execution: sandbox_runtime::NamespaceExecutionRuntimeConfig {
            scratch_root: root.join("command-scratch"),
            caps: sandbox_runtime::NamespaceExecutionCaps::default(),
        },
        layerstack: sandbox_runtime::LayerstackRuntimeConfig::default(),
        command: sandbox_runtime::CommandRuntimeConfig::default(),
        file: sandbox_runtime::FileRuntimeConfig::default(),
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
