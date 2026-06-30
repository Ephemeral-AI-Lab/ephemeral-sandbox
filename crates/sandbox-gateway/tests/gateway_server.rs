use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use sandbox_gateway::{GatewayConfig, SandboxGatewayServer};
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxManagerRouter, SandboxRecord,
    SandboxRuntime, SandboxState, SandboxStore, StartedDaemon,
};
use sandbox_protocol::{error_kind, CliOperationScope, Request, Response, MAX_REQUEST_BYTES};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct FakeRuntime;

impl SandboxRuntime for FakeRuntime {
    fn create_sandbox(
        &self,
        request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        let shared_base = request
            .shared_base
            .as_ref()
            .expect("manager create_sandbox passes shared base");
        assert_eq!(shared_base.target, PathBuf::from("/eos/layer-stack/base"));
        assert!(shared_base.readonly);
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
    invocations: Mutex<Vec<(u16, String, CliOperationScope)>>,
}

impl SandboxDaemonClient for RecordingDaemonClient {
    fn invoke_with_timeout(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: Request,
        _timeout: Duration,
    ) -> Result<Response, ManagerError> {
        self.invocations.lock().expect("invocations lock").push((
            endpoint.port,
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
    bind_addr: String,
    pid_path: PathBuf,
    max_concurrent_connections: usize,
    shutdown: CancellationToken,
) -> SandboxGatewayServer {
    SandboxGatewayServer::with_shutdown(
        GatewayConfig::new(bind_addr, pid_path, max_concurrent_connections, None),
        SandboxManagerRouter::new(services),
        shutdown,
    )
}

fn request(op: &str, scope: CliOperationScope, args: Value) -> Value {
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
        daemon_http: None,
        shared_base: None,
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

async fn send_value_lines(server: &SandboxGatewayServer, value: Value) -> Vec<String> {
    let mut raw = serde_json::to_vec(&value).expect("serialize request");
    raw.push(b'\n');
    send_raw_lines(server, &raw).await
}

async fn send_raw_lines(server: &SandboxGatewayServer, raw: &[u8]) -> Vec<String> {
    let (client, server_stream) = tokio::io::duplex(64 * 1024);
    let server_future = server.handle_connection(server_stream);
    let client_future = async {
        let (reader, mut writer) = tokio::io::split(client);
        let _ = writer.write_all(raw).await;
        let _ = writer.shutdown().await;

        let mut reader = BufReader::new(reader);
        let mut responses = Vec::new();
        loop {
            let mut response = String::new();
            let bytes = reader
                .read_line(&mut response)
                .await
                .expect("read response");
            if bytes == 0 {
                break;
            }
            responses.push(response);
        }
        responses
    };
    let (server_result, responses) = tokio::join!(server_future, client_future);
    server_result.expect("handle connection");
    responses
}

#[tokio::test]
async fn gateway_binds_tcp_writes_pid_file_and_cleans_up() -> TestResult {
    let root = unique_temp_dir("sandbox-gateway-server-test")?;
    let pid_path = root.join("gateway.pid");
    let bind_addr = reserve_local_addr()?;
    let shutdown = CancellationToken::new();
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        bind_addr.clone(),
        pid_path.clone(),
        8,
        shutdown.clone(),
    );
    let handle = tokio::spawn(server.serve());

    // The pid file is written only after the TCP listener binds, so its
    // presence proves the loopback port is live.
    wait_for_path(&pid_path).await?;
    assert_eq!(
        tokio::fs::read_to_string(&pid_path).await?,
        std::process::id().to_string()
    );
    wait_for_tcp(&bind_addr).await?;

    shutdown.cancel();
    handle.await??;
    assert!(!pid_path.exists());
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn gateway_connection_decodes_request_and_writes_response() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        "127.0.0.1:0".to_owned(),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );

    let response = send_value(
        &server,
        request("list_sandboxes", CliOperationScope::System, json!({})),
    )
    .await;

    assert_eq!(response["sandboxes"], json!([]));
    Ok(())
}

#[tokio::test]
async fn gateway_streams_create_sandbox_progress_before_final_response() -> TestResult {
    let root = unique_temp_dir("sandbox-gateway-create-test")?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("README.md"), b"base\n")?;
    let workspace_root = workspace.to_string_lossy().into_owned();
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        "127.0.0.1:0".to_owned(),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );
    let mut request = request(
        "create_sandbox",
        CliOperationScope::System,
        json!({"image": "ubuntu:24.04", "workspace_root": workspace_root.clone()}),
    );
    request["_stream_logs"] = json!(true);

    let responses = send_value_lines(&server, request).await;

    assert!(responses.len() > 1);
    assert!(responses[0].starts_with("cli_log("));
    assert!(responses
        .iter()
        .any(|response| response.contains("building shared workspace base")));
    assert!(responses.iter().any(
        |response| response.contains(&format!("creating runtime sandbox for {workspace_root}"))
    ));
    let final_response = serde_json::from_str::<Value>(responses.last().expect("final response"))?;
    assert_eq!(final_response["id"], "container-1");
    assert_eq!(
        final_response["workspace_root"].as_str(),
        Some(workspace_root.as_str())
    );
    assert_eq!(final_response["state"], "ready");
    assert!(final_response.get("event").is_none());
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[tokio::test]
async fn gateway_connection_rejects_oversized_request() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        "127.0.0.1:0".to_owned(),
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
        "127.0.0.1:0".to_owned(),
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
    let pid_path = root.join("gateway.pid");
    let bind_addr = reserve_local_addr()?;
    let shutdown = CancellationToken::new();
    let (services, _store, _daemon_client) = services();
    let server = server(
        services,
        bind_addr.clone(),
        pid_path.clone(),
        0,
        shutdown.clone(),
    );
    let handle = tokio::spawn(server.serve());

    wait_for_path(&pid_path).await?;
    let stream = connect_tcp(&bind_addr).await?;
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
async fn gateway_rejects_request_with_missing_or_wrong_auth_token() -> TestResult {
    let (services, _store, _daemon_client) = services();
    let server = SandboxGatewayServer::with_shutdown(
        GatewayConfig::new(
            "127.0.0.1:0",
            PathBuf::from("/tmp/test-gateway.pid"),
            8,
            Some("expected-token".to_owned()),
        ),
        SandboxManagerRouter::new(services),
        CancellationToken::new(),
    );

    let missing = send_value(
        &server,
        request("list_sandboxes", CliOperationScope::System, json!({})),
    )
    .await;
    assert_eq!(missing["error"]["kind"], error_kind::UNAUTHORIZED);

    let mut wrong = request("list_sandboxes", CliOperationScope::System, json!({}));
    wrong["_sandbox_gateway_auth_token"] = json!("nope");
    let wrong = send_value(&server, wrong).await;
    assert_eq!(wrong["error"]["kind"], error_kind::UNAUTHORIZED);

    let mut authorized = request("list_sandboxes", CliOperationScope::System, json!({}));
    authorized["_sandbox_gateway_auth_token"] = json!("expected-token");
    let authorized = send_value(&server, authorized).await;
    assert_eq!(authorized["sandboxes"], json!([]));
    Ok(())
}

#[tokio::test]
async fn gateway_dispatches_sandbox_scope_through_manager_router() -> TestResult {
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
    let server = server(
        services,
        "127.0.0.1:0".to_owned(),
        PathBuf::from("/tmp/test-gateway.pid"),
        8,
        CancellationToken::new(),
    );

    let response = send_value(
        &server,
        request(
            "exec_command",
            CliOperationScope::sandbox("sbox-1"),
            json!({"cmd": "pwd"}),
        ),
    )
    .await;

    assert_eq!(response["forwarded"], true);
    let invocations = daemon_client.invocations.lock().expect("invocations lock");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, 7000);
    assert_eq!(invocations[0].1, "exec_command");
    assert_eq!(invocations[0].2, CliOperationScope::sandbox("sbox-1"));
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

/// Bind an ephemeral loopback port, then release it so the gateway under test
/// can claim it. Standard test pattern; the tiny reuse window is acceptable.
fn reserve_local_addr() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

async fn connect_tcp(addr: &str) -> Result<TcpStream, Box<dyn std::error::Error + Send + Sync>> {
    for _ in 0..100 {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    Err(format!("timed out connecting to {addr}").into())
}

async fn wait_for_tcp(addr: &str) -> TestResult {
    connect_tcp(addr).await.map(|_| ())
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let mut label = prefix
        .split('-')
        .filter_map(|segment| segment.chars().next())
        .take(8)
        .collect::<String>();
    if label.is_empty() {
        label.push_str("tmp");
    }

    for _ in 0..1024 {
        let attempt = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("{label}-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(format!("failed to create unique temp dir for {prefix}").into())
}
