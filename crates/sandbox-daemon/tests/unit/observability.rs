// Daemon observability exposes live structural views and persisted operation
// events without collecting or retaining resource samples on request paths.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::observability::DaemonObservability;
use crate::rpc::{SandboxDaemonServer, ServerConfig};
use sandbox_config::configs::observability::ObservabilityConfig;
use sandbox_observability_telemetry::ObservabilityPaths;
use sandbox_operation_catalog::observability::{
    CGROUP_SPEC, EVENTS_SPEC, LAYERSTACK_SPEC, SNAPSHOT_SPEC, TRACE_SPEC,
};
use sandbox_runtime::workspace_session::FinalizePolicy;
use sandbox_runtime::{
    NamespaceExecutionId, NetworkProfile, RuntimeNamespaceExecutionSnapshot,
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, WorkspaceSessionId,
};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[test]
fn adapter_maps_concrete_runtime_snapshot_into_neutral_input() {
    let snapshot = crate::observability::adapter::map_snapshot(RuntimeObservabilitySnapshot {
        workspaces: vec![workspace_snapshot("workspace-1", None)],
        active_namespace_executions: vec![RuntimeNamespaceExecutionSnapshot {
            namespace_execution_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
            workspace_session_id: WorkspaceSessionId("workspace-1".to_owned()),
            operation_name: "exec_command".to_owned(),
            command: Some("printf ok".to_owned()),
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
    assert_eq!(
        snapshot.active_namespace_executions[0].command.as_deref(),
        Some("printf ok")
    );
}

#[test]
fn from_config_disabled_when_sandbox_id_is_missing() {
    let root = test_root("missing-sandbox-id");
    let config = server_config(&root, None);
    assert!(DaemonObservability::from_config(&config).is_none());
}

#[tokio::test]
async fn runtime_request_completion_does_not_create_resource_history() -> TestResult {
    let root = test_root("request-completion-purity");
    let server = daemon_server(&root, Some("sandbox-1"))?;

    let response = server
        .dispatch_bytes(
            request_bytes("unknown_runtime_op", "req-runtime", json!({}))?,
            false,
        )
        .await;
    assert_eq!(response, sandbox_operation_contract::OperationResponse::unknown_op());

    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;
    let samples = sandbox_observability_telemetry::Reader::new(
        paths.log_path().to_path_buf(),
        paths.rotated_log_path().to_path_buf(),
    )
    .samples("sandbox", 600_000);
    assert!(samples.is_empty(), "request completion retained resource history");
    Ok(())
}

#[tokio::test]
async fn snapshot_and_cgroup_reads_do_not_create_a_store() -> TestResult {
    let root = test_root("observability-read-purity");
    let server = daemon_server(&root, Some("sandbox-1"))?;
    let paths = ObservabilityPaths::from_socket_path(&server.config.socket_path)?;

    for (op, args) in [
        (SNAPSHOT_SPEC.name, json!({})),
        (CGROUP_SPEC.name, json!({ "scope": "sandbox" })),
    ] {
        let response = server
            .dispatch_bytes(request_bytes(op, "req-read", args)?, false)
            .await;
        assert!(response.as_json_value().get("error").is_none());
    }

    assert!(!paths.log_path().exists());
    assert!(!paths.rotated_log_path().exists());
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
    assert_eq!(snapshot["resources"]["latest"], Value::Null);
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
