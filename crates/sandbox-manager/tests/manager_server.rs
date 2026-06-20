use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_manager::{
    ManagerResult, ManagerServices, SandboxDaemonClient, SandboxDaemonEndpoint,
    SandboxDaemonInstaller, SandboxId, SandboxManagerServer, SandboxRecord, SandboxRuntime,
    SandboxState, SandboxStore, ServerConfig,
};
use sandbox_protocol::{
    error_kind, OperationAuthority, OperationCatalog, OperationScope, OperationSpec,
    SandboxRequest, SandboxResponse, MAX_REQUEST_BYTES,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static NEXT_SERVER_ID: AtomicUsize = AtomicUsize::new(1);
static TEST_DAEMON_SPECS: &[&OperationSpec] = &[];

#[derive(Default)]
struct FakeRuntime;

impl SandboxRuntime for FakeRuntime {
    fn create_sandbox(&self, _id: &SandboxId) -> ManagerResult<()> {
        Ok(())
    }

    fn destroy_sandbox(&self, _record: &SandboxRecord) -> ManagerResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeInstaller;

impl SandboxDaemonInstaller for FakeInstaller {
    fn start_daemon(&self, record: &SandboxRecord) -> ManagerResult<SandboxDaemonEndpoint> {
        Ok(SandboxDaemonEndpoint::new(
            PathBuf::from(format!("/tmp/{}.sock", record.id.as_str())),
            None,
        ))
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> ManagerResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDaemonClient {
    invocations: Mutex<Vec<(PathBuf, String, OperationScope)>>,
}

impl SandboxDaemonClient for RecordingDaemonClient {
    fn describe_operations(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
    ) -> ManagerResult<OperationCatalog> {
        Ok(OperationCatalog::new(
            OperationAuthority::SandboxDaemon,
            TEST_DAEMON_SPECS,
        ))
    }

    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: SandboxRequest,
    ) -> ManagerResult<SandboxResponse> {
        self.invocations.lock().expect("invocations lock").push((
            endpoint.socket_path.clone(),
            request.op.clone(),
            request.scope.clone(),
        ));
        Ok(SandboxResponse::ok(
            &request.as_request(),
            json!({
                "forwarded_op": request.op,
                "endpoint": endpoint.socket_path,
            }),
        ))
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

fn server(services: Arc<ManagerServices>) -> SandboxManagerServer {
    let id = NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "sandbox-manager-server-test-{}-{id}",
        std::process::id()
    ));
    SandboxManagerServer::new(
        ServerConfig::new(root.join("manager.sock"), root.join("manager.pid"), 8),
        services,
    )
}

fn request(op: &str, scope: OperationScope, args: Value) -> Value {
    json!({
        "op": op,
        "request_id": "req-1",
        "scope": scope,
        "args": args,
    })
}

fn sandbox_id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn ready_record(value: &str, daemon: Option<SandboxDaemonEndpoint>) -> SandboxRecord {
    SandboxRecord {
        id: sandbox_id(value),
        state: SandboxState::Ready,
        daemon,
    }
}

async fn send_value(server: &SandboxManagerServer, value: Value) -> Value {
    let mut raw = serde_json::to_vec(&value).expect("serialize request");
    raw.push(b'\n');
    send_raw(server, &raw).await
}

async fn send_raw(server: &SandboxManagerServer, raw: &[u8]) -> Value {
    let (client, server_stream) = tokio::io::duplex(1024);
    let server_future = server.handle_connection(server_stream);
    let client_future = async {
        let (reader, mut writer) = tokio::io::split(client);
        let _ = writer.write_all(raw).await;
        let _ = writer.shutdown().await;

        let mut response = String::new();
        let mut reader = BufReader::new(reader);
        reader
            .read_line(&mut response)
            .await
            .expect("read response");
        serde_json::from_str::<Value>(&response).expect("decode response")
    };
    let (server_result, response) = tokio::join!(server_future, client_future);
    server_result.expect("handle connection");
    response
}

#[tokio::test]
async fn manager_server_dispatches_system_manager_operation_locally() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);

    let response = send_value(
        &server,
        request("list_sandboxes", OperationScope::System, json!({})),
    )
    .await;

    assert_eq!(response["sandboxes"], json!([]));
}

#[tokio::test]
async fn manager_server_rejects_manager_operation_with_sandbox_scope() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);

    let response = send_value(
        &server,
        request(
            "list_sandboxes",
            OperationScope::sandbox("sbox-1"),
            json!({}),
        ),
    )
    .await;

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_server_unknown_system_operation_returns_unknown_op() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);

    let response = send_value(
        &server,
        request("exec_command", OperationScope::System, json!({})),
    )
    .await;

    assert_eq!(response["error"]["kind"], "unknown_op");
}

#[tokio::test]
async fn manager_server_forwards_sandbox_scoped_unknown_to_daemon_client() {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-1",
            Some(SandboxDaemonEndpoint::new("/tmp/sbox-1.sock", None)),
        ))
        .expect("insert sandbox");
    let server = server(services);

    let response = send_value(
        &server,
        request(
            "exec_command",
            OperationScope::sandbox("sbox-1"),
            json!({"cmd": "pwd"}),
        ),
    )
    .await;

    assert_eq!(response["forwarded_op"], "exec_command");
    assert_eq!(response["endpoint"], "/tmp/sbox-1.sock");
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, PathBuf::from("/tmp/sbox-1.sock"));
    assert_eq!(invocations[0].1, "exec_command");
    assert_eq!(invocations[0].2, OperationScope::sandbox("sbox-1"));
}

#[tokio::test]
async fn manager_server_rejects_sandbox_scope_when_sandbox_missing() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);

    let response = send_value(
        &server,
        request(
            "exec_command",
            OperationScope::sandbox("missing"),
            json!({}),
        ),
    )
    .await;

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_server_rejects_sandbox_scope_when_daemon_unavailable() {
    let (services, store, _daemon_client) = services();
    store
        .insert(ready_record("sbox-1", None))
        .expect("insert sandbox");
    let server = server(services);

    let response = send_value(
        &server,
        request("exec_command", OperationScope::sandbox("sbox-1"), json!({})),
    )
    .await;

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
}

#[tokio::test]
async fn manager_connection_rejects_bad_json() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);

    let response = send_raw(&server, b"{not json\n").await;

    assert_eq!(response["error"]["kind"], error_kind::BAD_JSON);
}

#[tokio::test]
async fn manager_connection_rejects_oversized_request() {
    let (services, _store, _daemon_client) = services();
    let server = server(services);
    let oversized = vec![b'a'; MAX_REQUEST_BYTES + 1];

    let response = send_raw(&server, &oversized).await;

    assert_eq!(response["error"]["kind"], error_kind::REQUEST_TOO_LARGE);
}
