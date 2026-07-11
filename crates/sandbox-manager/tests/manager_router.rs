use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use sandbox_manager::{
    manager_handler_keys, CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices,
    SandboxDaemonClient, SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId,
    SandboxManagerRouter, SandboxRecord, SandboxResourceMetrics, SandboxRuntime, SandboxState,
    SandboxStore, StartedDaemon,
};
use sandbox_operation_catalog::{
    internal,
    manager::EXPORT_CHANGES_SPEC,
    observability::{CGROUP_SPEC, SNAPSHOT_SPEC},
    routes,
};
use sandbox_operation_contract::{
    error, OperationExecutionOwner, OperationRequest, OperationResponse, OperationScope,
    OperationScopeKind, OperationVisibility,
};
use serde_json::{json, Value};

struct FakeRuntime {
    counters_available: bool,
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self {
            counters_available: true,
        }
    }
}

impl SandboxRuntime for FakeRuntime {
    fn list_images(&self) -> Result<Vec<String>, ManagerError> {
        Ok(vec!["ubuntu:24.04".to_owned()])
    }

    fn create_sandbox(
        &self,
        _request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        Ok(CreateSandboxResult {
            id: sandbox_id("container-1"),
        })
    }

    fn destroy_sandbox(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn read_sandbox_resource_metrics(
        &self,
        _id: &SandboxId,
    ) -> Result<SandboxResourceMetrics, ManagerError> {
        Ok(SandboxResourceMetrics {
            cpu_usage_usec: self.counters_available.then_some(42),
            memory_current_bytes: Some(1_024),
            memory_limit_bytes: Some(2_048),
            io_read_bytes: self.counters_available.then_some(4_096),
            io_write_bytes: self.counters_available.then_some(8_192),
        })
    }
}

struct FakeInstaller;

impl SandboxDaemonInstaller for FakeInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<StartedDaemon, ManagerError> {
        Ok(StartedDaemon {
            daemon: SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                format!("token-{}", record.id.as_str()),
            ),
            daemon_http: None,
        })
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn check_daemon(
        &self,
        _record: &SandboxRecord,
        _endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDaemonClient {
    invocations: Mutex<Vec<(u16, String, OperationScope)>>,
}

const EXPORTED_BYTES: &[u8] = b"phase-8-export";

impl SandboxDaemonClient for RecordingDaemonClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: OperationRequest,
        _timeout_override: Option<Duration>,
    ) -> Result<OperationResponse, ManagerError> {
        self.invocations.lock().expect("invocations lock").push((
            endpoint.port,
            request.op.clone(),
            request.scope.clone(),
        ));
        let result = match request.op.as_str() {
            internal::runtime::EXPORT_LAYERSTACK => json!({
                "export_id": "phase-8-export",
                "manifest_version": 3,
                "layers_exported": ["L000001-phase-8"],
                "entries": {
                    "files": 1,
                    "symlinks": 0,
                    "whiteouts": 0,
                    "opaques": 0,
                },
                "spool_bytes": EXPORTED_BYTES.len(),
            }),
            internal::runtime::READ_EXPORT_CHUNK => json!({
                "chunk": base64::engine::general_purpose::STANDARD.encode(EXPORTED_BYTES),
                "offset": 0,
                "len": EXPORTED_BYTES.len(),
                "total": EXPORTED_BYTES.len(),
                "eof": true,
            }),
            _ => json!({"forwarded": true}),
        };
        Ok(OperationResponse::ok(result))
    }
}

fn services() -> (
    Arc<ManagerServices>,
    Arc<SandboxStore>,
    Arc<RecordingDaemonClient>,
) {
    services_with_runtime(Arc::new(FakeRuntime::default()))
}

fn services_with_runtime(
    runtime: Arc<dyn SandboxRuntime>,
) -> (
    Arc<ManagerServices>,
    Arc<SandboxStore>,
    Arc<RecordingDaemonClient>,
) {
    let store = Arc::new(SandboxStore::new());
    let installer = Arc::new(FakeInstaller);
    let daemon_client = Arc::new(RecordingDaemonClient::default());
    let services = Arc::new(ManagerServices::new(
        Arc::clone(&store),
        runtime,
        installer,
        daemon_client.clone(),
    ));
    (services, store, daemon_client)
}

fn router(services: Arc<ManagerServices>) -> SandboxManagerRouter {
    SandboxManagerRouter::new(services)
}

fn request(op: &str, scope: OperationScope, args: Value) -> OperationRequest {
    OperationRequest::new(op, "req-1", scope, args)
}

fn sandbox_id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn ready_record(value: &str, daemon: Option<SandboxDaemonEndpoint>) -> SandboxRecord {
    SandboxRecord {
        id: sandbox_id(value),
        workspace_root: PathBuf::from("/testbed"),
        state: SandboxState::Ready,
        daemon,
        daemon_http: None,
        shared_base: None,
    }
}

fn ready_router() -> (SandboxManagerRouter, Arc<RecordingDaemonClient>) {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-1",
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-1",
            )),
        ))
        .expect("insert sandbox");
    (router(services), daemon_client)
}

#[tokio::test]
async fn manager_router_dispatches_system_manager_operation_locally() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request("list_sandboxes", OperationScope::System, json!({})))
        .await
        .into_json_value();

    assert_eq!(response["sandboxes"], json!([]));
}

#[tokio::test]
async fn manager_router_reads_sandbox_resource_metrics_from_the_runtime() {
    let (router, daemon_client) = ready_router();

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["view"], "cgroup");
    assert_eq!(response["scope"], "sandbox");
    assert_eq!(
        response["series"][0]["metrics"]["metrics_source"],
        "docker_engine"
    );
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
}

#[tokio::test]
async fn manager_router_does_not_report_unavailable_counters_as_zero() {
    let (services, store, daemon_client) = services_with_runtime(Arc::new(FakeRuntime {
        counters_available: false,
    }));
    store
        .insert(ready_record(
            "sbox-1",
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-1",
            )),
        ))
        .expect("insert sandbox");
    let router = router(services);

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    let metrics = &response["series"][0]["metrics"];
    assert_eq!(metrics["metrics_source"], "docker_engine");
    assert!(metrics.get("cpu_usec").is_none());
    assert!(metrics.get("io_rbytes").is_none());
    assert!(metrics.get("io_wbytes").is_none());
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
}

#[tokio::test]
async fn manager_router_forwards_workspace_resource_metrics_to_the_daemon() {
    let (router, daemon_client) = ready_router();

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox("sbox-1"),
            json!({ "scope": "workspace-1" }),
        ))
        .await
        .into_json_value();

    assert_eq!(response["forwarded"], true);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].1, CGROUP_SPEC.name);
}

#[test]
fn manager_public_routes_and_handler_keys_are_bijective() {
    let expected = routes::manager_routes()
        .iter()
        .chain(routes::observability_routes())
        .filter(|route| {
            route.execution_owner == OperationExecutionOwner::Manager
                && route.visibility == OperationVisibility::Public
        })
        .map(|route| (route.scope_kind, route.operation))
        .collect::<Vec<_>>();
    let actual = manager_handler_keys().collect::<Vec<_>>();

    assert_eq!(actual.len(), expected.len());
    for (scope_kind, operation) in &expected {
        assert_eq!(
            actual
                .iter()
                .filter(|(actual_scope, actual_operation)| {
                    actual_scope == scope_kind && actual_operation == operation
                })
                .count(),
            1,
            "public manager route ({scope_kind:?}, {operation}) must have one handler"
        );
    }
    for (scope_kind, operation) in &actual {
        assert_eq!(
            expected
                .iter()
                .filter(|(expected_scope, expected_operation)| {
                    expected_scope == scope_kind && expected_operation == operation
                })
                .count(),
            1,
            "manager handler ({scope_kind:?}, {operation}) must have one public route"
        );
    }
}

#[tokio::test]
async fn manager_router_routes_system_and_sandbox_snapshot_to_distinct_owners() {
    let (router, daemon_client) = ready_router();

    let system_route = routes::observability_routes()
        .iter()
        .find(|route| {
            route.operation == SNAPSHOT_SPEC.name && route.scope_kind == OperationScopeKind::System
        })
        .expect("system snapshot route");
    let sandbox_route = routes::observability_routes()
        .iter()
        .find(|route| {
            route.operation == SNAPSHOT_SPEC.name && route.scope_kind == OperationScopeKind::Sandbox
        })
        .expect("sandbox snapshot route");
    assert_eq!(
        system_route.execution_owner,
        OperationExecutionOwner::Manager
    );
    assert_eq!(
        sandbox_route.execution_owner,
        OperationExecutionOwner::Observability
    );

    let system_response = router
        .dispatch_request(request(
            SNAPSHOT_SPEC.name,
            OperationScope::System,
            json!({}),
        ))
        .await
        .into_json_value();
    assert_eq!(system_response["sandboxes"][0]["sandbox_id"], "sbox-1");

    let sandbox_response = router
        .dispatch_request(request(
            SNAPSHOT_SPEC.name,
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();
    assert_eq!(sandbox_response["forwarded"], true);

    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 2);
    assert!(invocations
        .iter()
        .all(|(_, operation, scope)| operation == SNAPSHOT_SPEC.name
            && scope == &OperationScope::sandbox("sbox-1")));
}

#[tokio::test]
async fn manager_router_rejects_manager_operation_with_sandbox_scope() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "list_sandboxes",
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_router_forwards_every_sandbox_observability_route() {
    let (router, daemon_client) = ready_router();

    let expected = routes::observability_routes()
        .iter()
        .filter(|route| route.execution_owner == OperationExecutionOwner::Observability)
        .map(|route| route.operation)
        .collect::<Vec<_>>();
    for operation in &expected {
        let response = router
            .dispatch_request(request(
                operation,
                OperationScope::sandbox("sbox-1"),
                json!({}),
            ))
            .await
            .into_json_value();
        assert_eq!(response["forwarded"], true, "{operation}");
    }

    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(
        invocations
            .iter()
            .map(|(_, operation, _)| operation.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(invocations
        .iter()
        .all(|(_, _, scope)| scope == &OperationScope::sandbox("sbox-1")));
}

#[tokio::test]
async fn manager_router_rejects_internal_routes_while_public_export_uses_direct_daemon_port() {
    let (router, daemon_client) = ready_router();

    for route in internal::runtime::ROUTES {
        assert_eq!(route.visibility, OperationVisibility::Internal);
        let scope = match route.scope_kind {
            OperationScopeKind::System => OperationScope::System,
            OperationScopeKind::Sandbox => OperationScope::sandbox("sbox-1"),
        };
        let response = router
            .dispatch_request(request(route.operation, scope, json!({})))
            .await
            .into_json_value();
        assert_eq!(
            response["error"]["kind"],
            error::INVALID_REQUEST,
            "{} must be rejected",
            route.operation
        );
    }

    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "manager-router-export-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create export directory");
    let destination = directory.join("delta.tar.zst");
    let response = router
        .dispatch_request(request(
            EXPORT_CHANGES_SPEC.name,
            OperationScope::System,
            json!({
                "sandbox_id": "sbox-1",
                "dest": destination,
                "format": "tar-zst",
            }),
        ))
        .await
        .into_json_value();

    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        std::fs::read(&destination).expect("read exported archive"),
        EXPORTED_BYTES
    );
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 2);
    assert_eq!(invocations[0].1, internal::runtime::EXPORT_LAYERSTACK);
    assert_eq!(invocations[1].1, internal::runtime::READ_EXPORT_CHUNK);
    assert!(invocations
        .iter()
        .all(|(port, _, scope)| *port == 7000 && scope == &OperationScope::sandbox("sbox-1")));
    drop(invocations);
    std::fs::remove_dir_all(directory).expect("remove export directory");
}

#[tokio::test]
async fn manager_router_rejects_file_list_gateway_rpc_before_forwarding() {
    let (router, daemon_client) = ready_router();

    let response = router
        .dispatch_request(request(
            internal::runtime::FILE_LIST,
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error::INVALID_REQUEST);
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
}

#[tokio::test]
async fn manager_router_unknown_system_operation_returns_unknown_op() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request("exec_command", OperationScope::System, json!({})))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], "unknown_op");
}

#[tokio::test]
async fn manager_router_forwards_sandbox_scoped_unknown_to_daemon_client() {
    let (router, daemon_client) = ready_router();

    let response = router
        .dispatch_request(request(
            "exec_command",
            OperationScope::sandbox("sbox-1"),
            json!({"cmd": "pwd"}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["forwarded"], true);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, 7000);
    assert_eq!(invocations[0].1, "exec_command");
    assert_eq!(invocations[0].2, OperationScope::sandbox("sbox-1"));
}

#[tokio::test]
async fn manager_router_rejects_sandbox_scope_when_sandbox_missing() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "exec_command",
            OperationScope::sandbox("missing"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_router_rejects_sandbox_scope_when_daemon_unavailable() {
    let (services, store, _daemon_client) = services();
    store
        .insert(ready_record("sbox-1", None))
        .expect("insert sandbox");
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "exec_command",
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error::INVALID_REQUEST);
}
