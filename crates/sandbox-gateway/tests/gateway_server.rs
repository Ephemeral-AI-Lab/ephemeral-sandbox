use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_gateway::{GatewayConfig, SandboxGatewayServer};
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxManagerRouter, SandboxRecord,
    SandboxRuntime, SandboxState, SandboxStore,
};
use sandbox_protocol::{
    error_kind, CliOperationSpec, OperationCatalog, OperationExecutionSpace, OperationFamilySpec,
    OperationScope, Request, Response, MAX_REQUEST_BYTES,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

static TEST_DAEMON_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "test",
    title: "Test",
    summary: "Test runtime operations.",
    description: "Test runtime operations.",
};

static TEST_DAEMON_FAMILIES: &[&OperationFamilySpec] = &[&TEST_DAEMON_FAMILY];
static TEST_DAEMON_SPECS: &[&CliOperationSpec] = &[];

#[derive(Default)]
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

#[derive(Default)]
struct FakeInstaller;

impl SandboxDaemonInstaller for FakeInstaller {
    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        Ok(SandboxDaemonEndpoint::new(
            PathBuf::from(format!("/tmp/{}.sock", record.id.as_str())),
            None,
        ))
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
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
    ) -> Result<OperationCatalog, ManagerError> {
        Ok(OperationCatalog::new(
            OperationExecutionSpace::Runtime,
            TEST_DAEMON_FAMILIES,
            TEST_DAEMON_SPECS,
        ))
    }

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

fn server(
    services: Arc<ManagerServices>,
    socket_path: PathBuf,
    pid_path: PathBuf,
    max_concurrent_connections: usize,
    shutdown: CancellationToken,
) -> SandboxGatewayServer {
    SandboxGatewayServer::with_shutdown(
        GatewayConfig::new(socket_path, pid_path, max_concurrent_connections),
        SandboxManagerRouter::new(services),
        shutdown,
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
        workspace_root: PathBuf::from("/testbed"),
        state: SandboxState::Ready,
        daemon,
    }
}

async fn send_value(server: &SandboxGatewayServer, value: Value) -> Value {
    let mut raw = serde_json::to_vec(&value).expect("serialize request");
    raw.push(b'\n');
    send_raw(server, &raw).await
}

async fn send_raw(server: &SandboxGatewayServer, raw: &[u8]) -> Value {
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
async fn gateway_binds_socket_writes_pid_file_and_cleans_up() -> TestResult {
    let root = unique_temp_dir("sandbox-gateway-server-test")?;
    let socket_path = root.join("gateway.sock");
    let pid_path = root.join("gateway.pid");
    let shutdown = CancellationToken::new();
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        socket_path.clone(),
        pid_path.clone(),
        8,
        shutdown.clone(),
    );
    let handle = tokio::spawn(server.serve());

    wait_for_path(&socket_path).await?;
    wait_for_path(&pid_path).await?;
    assert_eq!(
        tokio::fs::read_to_string(&pid_path).await?,
        std::process::id().to_string()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = tokio::fs::metadata(&socket_path)
            .await?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    shutdown.cancel();
    handle.await??;
    assert!(!socket_path.exists());
    assert!(!pid_path.exists());
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn gateway_connection_decodes_request_and_writes_response() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        PathBuf::from("/tmp/test-gateway.sock"),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );

    let response = send_value(
        &server,
        request("list_sandboxes", OperationScope::System, json!({})),
    )
    .await;

    assert_eq!(response["sandboxes"], json!([]));
    Ok(())
}

#[tokio::test]
async fn gateway_connection_rejects_oversized_request() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        PathBuf::from("/tmp/test-gateway.sock"),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );
    let oversized = vec![b'a'; MAX_REQUEST_BYTES + 1];

    let response = send_raw(&server, &oversized).await;

    assert_eq!(response["error"]["kind"], error_kind::REQUEST_TOO_LARGE);
    assert_eq!(response["error"]["details"]["limit"], MAX_REQUEST_BYTES);
    Ok(())
}

#[tokio::test]
async fn gateway_connection_rejects_missing_newline() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        PathBuf::from("/tmp/test-gateway.sock"),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );

    let response = send_raw(&server, br#"{"op":"list_sandboxes"}"#).await;

    assert_eq!(response["error"]["kind"], error_kind::INVALID_REQUEST);
    Ok(())
}

#[tokio::test]
async fn gateway_overload_response_is_structured_json() -> TestResult {
    let root = unique_temp_dir("sandbox-gateway-overload-test")?;
    let socket_path = root.join("gateway.sock");
    let pid_path = root.join("gateway.pid");
    let shutdown = CancellationToken::new();
    let (services, _store, _daemon_client) = services();
    let server = server(services, socket_path.clone(), pid_path, 0, shutdown.clone());
    let handle = tokio::spawn(server.serve());

    wait_for_path(&socket_path).await?;
    let stream = UnixStream::connect(&socket_path).await?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).await?;
    let response = serde_json::from_str::<Value>(&response)?;

    assert_eq!(response["error"]["kind"], error_kind::INTERNAL_ERROR);
    assert_eq!(
        response["error"]["details"]["max_concurrent_connections"],
        0
    );

    shutdown.cancel();
    handle.await??;
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn gateway_dispatches_sandbox_scope_through_manager_router() -> TestResult {
    let (services, store, daemon_client) = services();
    store
        .insert(ready_record(
            "sbox-1",
            Some(SandboxDaemonEndpoint::new("/tmp/sbox-1.sock", None)),
        ))
        .expect("insert sandbox");
    let server = server(
        services,
        PathBuf::from("/tmp/test-gateway.sock"),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );

    let response = send_value(
        &server,
        request(
            "exec_command",
            OperationScope::sandbox("sbox-1"),
            json!({"cmd": "pwd"}),
        ),
    )
    .await;

    assert_eq!(response["forwarded"], true);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, PathBuf::from("/tmp/sbox-1.sock"));
    assert_eq!(invocations[0].1, "exec_command");
    assert_eq!(invocations[0].2, OperationScope::sandbox("sbox-1"));
    Ok(())
}

async fn wait_for_path(path: &std::path::Path) -> TestResult {
    for _ in 0..100 {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Err(format!("timed out waiting for {}", path.display()).into())
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let short_prefix = prefix.chars().take(3).collect::<String>();
    Ok(std::env::temp_dir().join(format!("{short_prefix}-{}-{nanos:x}", std::process::id())))
}
