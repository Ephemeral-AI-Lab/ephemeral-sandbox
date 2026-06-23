use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxManagerRouter, SandboxRecord,
    SandboxRuntime, SandboxState, SandboxStore,
};
use sandbox_protocol::{error_kind, CliOperationScope, Request, Response};
use serde_json::{json, Value};

struct FakeRuntime;

impl SandboxRuntime for FakeRuntime {
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
}

struct FakeInstaller;

impl SandboxDaemonInstaller for FakeInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        Ok(SandboxDaemonEndpoint::new(
            PathBuf::from(format!("/tmp/{}.sock", record.id.as_str())),
            None,
        ))
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn check_daemon(&self, _endpoint: &SandboxDaemonEndpoint) -> Result<(), ManagerError> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDaemonClient {
    invocations: Mutex<Vec<(PathBuf, String, CliOperationScope)>>,
}

impl SandboxDaemonClient for RecordingDaemonClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: Request,
    ) -> Result<Response, ManagerError> {
        self.invocations.lock().expect("invocations lock").push((
            endpoint.socket_path.clone(),
            request.op.clone(),
            request.scope.clone(),
        ));
        Ok(Response::ok(json!({"forwarded": true})))
    }
}

fn services() -> (
    Arc<ManagerServices>,
    Arc<SandboxStore>,
    Arc<RecordingDaemonClient>,
) {
    let store = Arc::new(SandboxStore::new());
    let runtime = Arc::new(FakeRuntime);
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

fn request(op: &str, scope: CliOperationScope, args: Value) -> Request {
    Request::new(op, "req-1", scope, args)
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
    }
}

#[tokio::test]
async fn manager_router_dispatches_system_manager_operation_locally() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "list_sandboxes",
            CliOperationScope::System,
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["sandboxes"], json!([]));
}

#[tokio::test]
async fn manager_router_rejects_manager_operation_with_sandbox_scope() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "list_sandboxes",
            CliOperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_router_unknown_system_operation_returns_unknown_op() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "exec_command",
            CliOperationScope::System,
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], "unknown_op");
}

#[tokio::test]
async fn manager_router_forwards_sandbox_scoped_unknown_to_daemon_client() {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-1",
            Some(SandboxDaemonEndpoint::new("/tmp/sbox-1.sock", None)),
        ))
        .expect("insert sandbox");
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "exec_command",
            CliOperationScope::sandbox("sbox-1"),
            json!({"cmd": "pwd"}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["forwarded"], true);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, PathBuf::from("/tmp/sbox-1.sock"));
    assert_eq!(invocations[0].1, "exec_command");
    assert_eq!(invocations[0].2, CliOperationScope::sandbox("sbox-1"));
}

#[tokio::test]
async fn manager_router_rejects_sandbox_scope_when_sandbox_missing() {
    let (services, _store, _daemon_client) = services();
    let router = router(services);

    let response = router
        .dispatch_request(request(
            "exec_command",
            CliOperationScope::sandbox("missing"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
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
            CliOperationScope::sandbox("sbox-1"),
            json!({}),
        ))
        .await
        .into_json_value();

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
}
