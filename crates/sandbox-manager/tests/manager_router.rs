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
    observability::{CGROUP_SPEC, DAEMON_SPEC, SNAPSHOT_SPEC},
    routes,
    runtime::{
        CREATE_WORKSPACE_SESSION_SPEC, DESTROY_WORKSPACE_SESSION_SPEC, EXEC_COMMAND_SPEC,
        FILE_BLAME_SPEC, FILE_EDIT_SPEC, FILE_READ_SPEC, FILE_WRITE_SPEC,
        PUBLISH_WORKSPACE_SESSION_SPEC, READ_LINES_SPEC, WRITE_STDIN_SPEC,
    },
};
use sandbox_operation_contract::{
    error, OperationExecutionOwner, OperationRequest, OperationResponse, OperationScope,
    OperationScopeKind, OperationVisibility,
};
use serde_json::{json, Value};

struct FakeRuntime {
    counters_available: bool,
    resource_batches: AtomicU64,
    resource_reads: AtomicU64,
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self {
            counters_available: true,
            resource_batches: AtomicU64::new(0),
            resource_reads: AtomicU64::new(0),
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
            resource_profile: None,
        })
    }

    fn destroy_sandbox(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn read_sandbox_resource_metrics(
        &self,
        _id: &SandboxId,
    ) -> Result<SandboxResourceMetrics, ManagerError> {
        self.resource_reads.fetch_add(1, Ordering::SeqCst);
        Ok(SandboxResourceMetrics {
            cpu_usage_usec: self.counters_available.then_some(42),
            memory_current_bytes: Some(1_024),
            memory_limit_bytes: Some(2_048),
            io_read_bytes: self.counters_available.then_some(4_096),
            io_write_bytes: self.counters_available.then_some(8_192),
        })
    }

    fn read_sandbox_resource_metrics_batch(
        &self,
        ids: &[SandboxId],
    ) -> Vec<(SandboxId, Result<SandboxResourceMetrics, ManagerError>)> {
        self.resource_batches.fetch_add(1, Ordering::SeqCst);
        ids.iter()
            .cloned()
            .map(|id| {
                let metrics = self.read_sandbox_resource_metrics(&id);
                (id, metrics)
            })
            .collect()
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
const RESOURCES_OPERATION: &str = "resources";
const TOPOLOGY_OPERATION: &str = "topology";

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
            "cgroup" => json!({
                "forwarded": true,
                "topology": {
                    "schema_version": 2,
                    "available": true,
                    "source": "proc_namespaces",
                    "error": null,
                    "truncated": false,
                    "warnings": [],
                    "workspaces": [{
                        "workspace_id": "workspace-1",
                        "state": "idle",
                        "holder_pid": 41,
                        "pid_namespace": "pid:[100]",
                        "mount_namespace": "mnt:[200]",
                        "processes": [],
                    }],
                },
            }),
            "topology" => json!({
                "forwarded": true,
                "view": "topology",
                "scope": "sandbox",
                "topology": {
                    "schema_version": 2,
                    "available": true,
                    "source": "proc_namespaces",
                    "error": null,
                    "truncated": false,
                    "warnings": [],
                    "workspaces": [{
                        "workspace_id": "workspace-1",
                        "state": "idle",
                        "holder_pid": 41,
                        "pid_namespace": "pid:[100]",
                        "mount_namespace": "mnt:[200]",
                        "processes": [],
                    }],
                },
            }),
            "daemon" => json!({
                "forwarded": true,
                "view": "daemon",
                "scope": "sandbox",
                "daemon": {
                    "available": true,
                    "pid": 42,
                    "thread_count": 8,
                },
            }),
            "resources" => json!({
                "view": "resources",
                "scope": "sandbox",
                "sandbox_id": request.scope.sandbox_id(),
                "source": "daemon_disk",
                "availability": "available",
                "errors": [],
                "series": [{
                    "ts": 1,
                    "scope": "sandbox",
                    "metrics": {"fixture_marker": "daemon-response"},
                    "deltas": {},
                    "sample_delta_ms": null,
                }],
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
        activity_revision: 0,
        daemon,
        daemon_http: None,
        shared_base: None,
        resource_profile: None,
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
async fn manager_router_merges_host_resource_metrics_with_daemon_topology() {
    let sandbox = sandbox_id("sbox-host-ring");
    let runtime = Arc::new(FakeRuntime::default());
    let (services, store, daemon_client) = services_with_runtime(runtime.clone());
    store
        .insert(ready_record(
            sandbox.as_str(),
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-host-ring",
            )),
        ))
        .expect("insert sandbox");
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("remove stale resource ring");
    assert_eq!(services.sample_resources_once(), 1);
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
    let router = router(Arc::clone(&services));

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox(sandbox.as_str()),
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
    assert_eq!(response["topology"]["schema_version"], 2);
    assert_eq!(response["topology"]["available"], true);
    assert_eq!(response["topology"]["source"], "proc_namespaces");
    assert_eq!(
        response["topology"]["workspaces"][0]["workspace_id"],
        "workspace-1"
    );
    assert_eq!(runtime.resource_reads.load(Ordering::SeqCst), 1);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].1, CGROUP_SPEC.name);
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("clean resource ring");
}

#[tokio::test]
async fn manager_router_does_not_report_unavailable_counters_as_zero() {
    let sandbox = sandbox_id("sbox-unavailable-counters");
    let runtime = Arc::new(FakeRuntime {
        counters_available: false,
        ..FakeRuntime::default()
    });
    let (services, store, daemon_client) = services_with_runtime(runtime.clone());
    store
        .insert(ready_record(
            sandbox.as_str(),
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-unavailable-counters",
            )),
        ))
        .expect("insert sandbox");
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("remove stale resource ring");
    assert_eq!(services.sample_resources_once(), 1);
    let router = router(Arc::clone(&services));

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox(sandbox.as_str()),
            json!({}),
        ))
        .await
        .into_json_value();

    let metrics = &response["series"][0]["metrics"];
    assert_eq!(metrics["metrics_source"], "docker_engine");
    assert!(metrics.get("cpu_usec").is_none());
    assert!(metrics.get("io_rbytes").is_none());
    assert!(metrics.get("io_wbytes").is_none());
    assert_eq!(response["topology"]["available"], true);
    assert_eq!(runtime.resource_reads.load(Ordering::SeqCst), 1);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].1, CGROUP_SPEC.name);
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("clean resource ring");
}

#[tokio::test]
async fn manager_preserves_resource_series_when_topology_transport_is_unavailable() {
    let sandbox = sandbox_id("sbox-no-daemon");
    let runtime = Arc::new(FakeRuntime::default());
    let (services, store, daemon_client) = services_with_runtime(runtime);
    store
        .insert(ready_record(sandbox.as_str(), None))
        .expect("insert sandbox");
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("remove stale resource ring");
    assert_eq!(services.sample_resources_once(), 1);
    let router = router(Arc::clone(&services));

    let response = router
        .dispatch_request(request(
            CGROUP_SPEC.name,
            OperationScope::sandbox(sandbox.as_str()),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["series"].as_array().map(Vec::len), Some(1));
    assert_eq!(response["topology"]["schema_version"], 2);
    assert_eq!(response["topology"]["available"], false);
    assert_eq!(response["topology"]["workspaces"], json!([]));
    assert_eq!(
        response["topology"]["error"],
        "sandbox daemon topology unavailable: sandbox daemon unavailable for sbox-no-daemon"
    );
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("clean resource ring");
}

#[tokio::test]
async fn manager_single_and_fleet_resource_reads_are_daemon_quiescent_for_ten_thousand_iterations()
{
    let runtime = Arc::new(FakeRuntime::default());
    let (services, store, daemon_client) = services_with_runtime(runtime.clone());
    for (id, port) in [("sbox-pure", 7000), ("sbox-peer", 7001)] {
        store
            .insert(ready_record(
                id,
                Some(SandboxDaemonEndpoint::new(
                    "127.0.0.1",
                    port,
                    format!("token-{id}"),
                )),
            ))
            .expect("insert ready sandbox");
        services
            .resource_ring()
            .remove(&sandbox_id(id))
            .expect("remove stale resource ring");
    }
    let mut not_ready = ready_record("sbox-creating", None);
    not_ready.state = SandboxState::Creating;
    store.insert(not_ready).expect("insert non-ready sandbox");
    assert_eq!(services.sample_resources_once(), 2);
    let manager_router = router(Arc::clone(&services));

    let single_resources = request(
        RESOURCES_OPERATION,
        OperationScope::sandbox("sbox-pure"),
        json!({ "window_ms": 600_000 }),
    );
    let fleet_resources = request(RESOURCES_OPERATION, OperationScope::System, json!({}));
    let ring_paths = ["sbox-pure", "sbox-peer"].map(|id| {
        let path = services.resource_ring().path(&sandbox_id(id));
        let contents = std::fs::read(&path).expect("read ring before pure reads");
        let metadata = std::fs::metadata(&path).expect("ring metadata before pure reads");
        (path, contents, metadata)
    });

    let sampled = manager_router
        .dispatch_request(single_resources.clone())
        .await
        .into_json_value();
    assert_eq!(sampled["view"], "resources");
    assert_eq!(sampled["scope"], "sandbox");
    assert_eq!(sampled["sandbox_id"], "sbox-pure");
    assert_eq!(sampled["source"], "daemon_disk");
    assert_eq!(
        sampled["series"][0]["metrics"]["fixture_marker"],
        "daemon-response"
    );
    {
        let invocations = daemon_client.invocations.lock().expect("invocations lock");
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].1, RESOURCES_OPERATION);
        assert_eq!(invocations[0].2, OperationScope::sandbox("sbox-pure"));
    }

    for _ in 0..10_000 {
        let fleet = manager_router
            .dispatch_request(fleet_resources.clone())
            .await
            .into_json_value();
        assert_eq!(fleet["view"], "resources");
        assert_eq!(fleet["scope"], "fleet");
        let sandboxes = fleet["sandboxes"].as_object().expect("fleet map");
        assert_eq!(sandboxes.len(), 2);
        assert!(sandboxes.contains_key("sbox-pure"));
        assert!(sandboxes.contains_key("sbox-peer"));
        assert!(!sandboxes.contains_key("sbox-creating"));
        assert!(sandboxes.values().all(|entry| !entry["current"].is_null()));
    }

    for (path, before, before_metadata) in ring_paths {
        let after = std::fs::read(&path).expect("read ring after pure reads");
        let after_metadata = std::fs::metadata(&path).expect("ring metadata after pure reads");
        assert_eq!(after, before, "pure reads must not change ring contents");
        assert_eq!(after_metadata.len(), before_metadata.len());
        assert_eq!(
            after_metadata.modified().ok(),
            before_metadata.modified().ok()
        );
    }
    assert_eq!(runtime.resource_reads.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.resource_batches.load(Ordering::SeqCst), 1);
    assert_eq!(
        daemon_client
            .invocations
            .lock()
            .expect("invocations lock")
            .len(),
        1,
        "system resources must remain manager-owned"
    );

    let topology = manager_router
        .dispatch_request(request(
            TOPOLOGY_OPERATION,
            OperationScope::sandbox("sbox-pure"),
            json!({}),
        ))
        .await
        .into_json_value();
    assert_eq!(topology["view"], "topology");
    assert_eq!(topology["scope"], "sandbox");
    assert_eq!(topology["topology"]["schema_version"], 2);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 2);
    assert_eq!(invocations[1].1, TOPOLOGY_OPERATION);
    drop(invocations);

    for id in ["sbox-pure", "sbox-peer"] {
        services
            .resource_ring()
            .remove(&sandbox_id(id))
            .expect("clean resource ring");
    }
}

#[tokio::test]
async fn manager_forwards_one_daemon_self_read_without_topology() {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-daemon-self",
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-daemon-self",
            )),
        ))
        .expect("insert sandbox");

    let response = router(Arc::clone(&services))
        .dispatch_request(request(
            DAEMON_SPEC.name,
            OperationScope::sandbox("sbox-daemon-self"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["view"], "daemon");
    assert_eq!(response["daemon"]["pid"], 42);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].1, DAEMON_SPEC.name);
    assert!(invocations
        .iter()
        .all(|(_, operation, _)| operation != TOPOLOGY_OPERATION));
}

#[tokio::test]
async fn sandbox_resources_require_a_daemon_endpoint_and_never_fall_back_to_the_ring() {
    let sandbox = sandbox_id("sbox-resource-only");
    let runtime = Arc::new(FakeRuntime::default());
    let (services, store, daemon_client) = services_with_runtime(runtime.clone());
    store
        .insert(ready_record(sandbox.as_str(), None))
        .expect("insert sandbox");
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("remove stale resource ring");
    assert_eq!(services.sample_resources_once(), 1);

    let response = router(Arc::clone(&services))
        .dispatch_request(request(
            RESOURCES_OPERATION,
            OperationScope::sandbox(sandbox.as_str()),
            json!({}),
        ))
        .await
        .into_json_value();

    assert!(
        response.get("error").is_some(),
        "unexpected response: {response}"
    );
    assert_eq!(runtime.resource_reads.load(Ordering::SeqCst), 1);
    assert!(daemon_client
        .invocations
        .lock()
        .expect("invocations lock")
        .is_empty());
    services
        .resource_ring()
        .remove(&sandbox)
        .expect("clean resource ring");
}

#[tokio::test]
async fn activity_revision_advances_only_after_successful_daemon_mutations() {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-revision",
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-revision",
            )),
        ))
        .expect("insert sandbox");
    let router = router(services);

    for operation in [
        FILE_READ_SPEC.name,
        FILE_BLAME_SPEC.name,
        READ_LINES_SPEC.name,
        SNAPSHOT_SPEC.name,
    ] {
        let response = router
            .dispatch_request(request(
                operation,
                OperationScope::sandbox("sbox-revision"),
                json!({}),
            ))
            .await
            .into_json_value();
        assert!(response.get("error").is_none(), "{operation}: {response}");
        assert_eq!(
            store
                .inspect(&sandbox_id("sbox-revision"))
                .expect("inspect sandbox")
                .activity_revision,
            0
        );
    }

    for (expected, operation) in [
        EXEC_COMMAND_SPEC.name,
        WRITE_STDIN_SPEC.name,
        FILE_WRITE_SPEC.name,
        FILE_EDIT_SPEC.name,
        CREATE_WORKSPACE_SESSION_SPEC.name,
        PUBLISH_WORKSPACE_SESSION_SPEC.name,
        DESTROY_WORKSPACE_SESSION_SPEC.name,
    ]
    .into_iter()
    .enumerate()
    {
        let response = router
            .dispatch_request(request(
                operation,
                OperationScope::sandbox("sbox-revision"),
                json!({}),
            ))
            .await
            .into_json_value();
        assert!(response.get("error").is_none(), "{operation}: {response}");
        assert_eq!(
            store
                .inspect(&sandbox_id("sbox-revision"))
                .expect("inspect sandbox")
                .activity_revision,
            (expected + 1) as u64,
            "{operation}"
        );
    }

    assert_eq!(
        daemon_client
            .invocations
            .lock()
            .expect("invocations lock")
            .len(),
        11
    );
}

#[tokio::test]
async fn failed_daemon_mutation_does_not_advance_activity_revision() {
    struct FaultingClient;

    impl SandboxDaemonClient for FaultingClient {
        fn invoke(
            &self,
            _endpoint: &SandboxDaemonEndpoint,
            _request: OperationRequest,
            _timeout_override: Option<Duration>,
        ) -> Result<OperationResponse, ManagerError> {
            Ok(OperationResponse::fault(
                error::OPERATION_FAILED,
                "mutation rejected",
            ))
        }
    }

    let store = Arc::new(SandboxStore::new());
    store
        .insert(ready_record(
            "sbox-failed-revision",
            Some(SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                "token-sbox-failed-revision",
            )),
        ))
        .expect("insert sandbox");
    let services = Arc::new(ManagerServices::new(
        Arc::clone(&store),
        Arc::new(FakeRuntime::default()),
        Arc::new(FakeInstaller),
        Arc::new(FaultingClient),
    ));
    let response = router(services)
        .dispatch_request(request(
            EXEC_COMMAND_SPEC.name,
            OperationScope::sandbox("sbox-failed-revision"),
            json!({ "cmd": "false" }),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error::OPERATION_FAILED);
    assert_eq!(
        store
            .inspect(&sandbox_id("sbox-failed-revision"))
            .expect("inspect sandbox")
            .activity_revision,
        0
    );
}

#[tokio::test]
async fn publish_activity_revision_advances_only_after_publish_completion() {
    struct PublishOutcomeClient {
        response: OperationResponse,
    }

    impl SandboxDaemonClient for PublishOutcomeClient {
        fn invoke(
            &self,
            _endpoint: &SandboxDaemonEndpoint,
            _request: OperationRequest,
            _timeout_override: Option<Duration>,
        ) -> Result<OperationResponse, ManagerError> {
            Ok(self.response.clone())
        }
    }

    let cases = [
        (
            "pre-commit-rejection",
            OperationResponse::fault_with_details(
                error::OPERATION_FAILED,
                "workspace session publish was rejected",
                json!({
                    "workspace_session_id": "workspace-1",
                    "stage": "publish",
                    "session_retained": true,
                    "publish_rejection": {
                        "path": "notes.txt",
                        "reason": "source_conflict"
                    }
                }),
            ),
            0,
        ),
        (
            "committed-close-failure",
            OperationResponse::fault_with_details(
                error::OPERATION_FAILED,
                "workspace session published but could not be closed",
                json!({
                    "workspace_session_id": "workspace-1",
                    "stage": "destroy",
                    "publish_completed": true,
                    "layer_committed": true,
                    "destroyed": false,
                    "session_state": "finalize_failed"
                }),
            ),
            1,
        ),
        (
            "no-op-close-failure",
            OperationResponse::fault_with_details(
                error::OPERATION_FAILED,
                "workspace session published but could not be closed",
                json!({
                    "workspace_session_id": "workspace-1",
                    "stage": "destroy",
                    "publish_completed": true,
                    "layer_committed": false,
                    "destroyed": false,
                    "session_state": "finalize_failed"
                }),
            ),
            1,
        ),
    ];

    for (id, response, expected_revision) in cases {
        let store = Arc::new(SandboxStore::new());
        store
            .insert(ready_record(
                id,
                Some(SandboxDaemonEndpoint::new("127.0.0.1", 7000, "token")),
            ))
            .expect("insert sandbox");
        let services = Arc::new(ManagerServices::new(
            Arc::clone(&store),
            Arc::new(FakeRuntime::default()),
            Arc::new(FakeInstaller),
            Arc::new(PublishOutcomeClient { response }),
        ));

        let response = router(services)
            .dispatch_request(request(
                PUBLISH_WORKSPACE_SESSION_SPEC.name,
                OperationScope::sandbox(id),
                json!({"workspace_session_id": "workspace-1"}),
            ))
            .await
            .into_json_value();

        assert_eq!(response["error"]["kind"], error::OPERATION_FAILED);
        assert_eq!(
            store
                .inspect(&sandbox_id(id))
                .expect("inspect sandbox")
                .activity_revision,
            expected_revision,
            "{id}"
        );
    }
}

#[tokio::test]
async fn manager_router_preserves_legacy_workspace_cgroup_forwarding() {
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
    assert_eq!(invocations[0].2, OperationScope::sandbox("sbox-1"));
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
        if *operation == RESOURCES_OPERATION {
            assert_eq!(response["source"], "daemon_disk", "{operation}");
            assert_eq!(
                response["series"][0]["metrics"]["fixture_marker"], "daemon-response",
                "{operation}"
            );
            assert!(
                response.get("forwarded").is_none(),
                "the manager must not rewrite the daemon response"
            );
        } else {
            assert_eq!(response["forwarded"], true, "{operation}");
        }
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
