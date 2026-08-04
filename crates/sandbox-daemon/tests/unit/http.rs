use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use http_body_util::BodyExt as _;
use sandbox_config::configs::daemon::DaemonHttpForwardConfig;
use sandbox_config::configs::observability::ObservabilityConfig;
use sandbox_observability_telemetry::Observer;
use sandbox_protocol::ProtocolLimits;
use sandbox_runtime::command::{CommandConfig, CommandOperationService};
use sandbox_runtime::file::FileService;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::workspace_session::{
    CreateSessionRequest, FinalizePolicy, WorkspaceSessionService,
};
use sandbox_runtime::{LayerstackRuntimeConfig, SandboxRuntimeOperations};
use sandbox_runtime_layerstack::{manifest_root_hash, LayerChange, LayerPath, LayerStack};
use sandbox_runtime_workspace::{
    run_result_ok, CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest,
    FileRunnerDirEntry, FileRunnerDirEntryKind, FileRunnerResult, HolderFinalization, HolderProbe,
    LayerStackSnapshotRef, LeaseId, NetworkProfile, WorkspaceError, WorkspaceHandle, WorkspaceRuntimeHooks,
    WorkspaceRuntimeService, WorkspaceSessionId,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::rpc::{ConnectionAdmission, ServerConfig};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct HttpTestServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
    root: PathBuf,
    operations: Arc<SandboxRuntimeOperations>,
    connection_admission: ConnectionAdmission,
    async_tasks: TaskTracker,
}

impl HttpTestServer {
    async fn start() -> TestResult<Self> {
        Self::start_with_admission(8, 256).await
    }

    async fn start_with_admission(
        max_blocking_requests: usize,
        max_concurrent_connections: usize,
    ) -> TestResult<Self> {
        let root = test_root("server");
        let operations = test_operations(&root)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let connection_admission = ConnectionAdmission::new(max_concurrent_connections);
        let async_tasks = TaskTracker::new();
        let task = crate::http::spawn(
            listener,
            server_config(&root),
            Arc::clone(&operations),
            Observer::disabled(),
            crate::rpc::BlockingAdmission::new(max_blocking_requests),
            connection_admission.clone(),
            async_tasks.clone(),
            shutdown.clone(),
        );
        Ok(Self {
            addr,
            shutdown,
            task,
            root,
            operations,
            connection_admission,
            async_tasks,
        })
    }

    async fn stop(self) -> TestResult {
        self.shutdown.cancel();
        self.task.await?;
        self.async_tasks.close();
        timeout(Duration::from_secs(1), self.async_tasks.wait())
            .await
            .expect("HTTP tasks drained");
        std::fs::remove_dir_all(self.root)?;
        Ok(())
    }
}

struct RawResponse {
    status: u16,
    head: String,
    body: Vec<u8>,
}

#[tokio::test]
async fn health_is_fixed_without_runtime_initialization() -> TestResult {
    let response = crate::http::health::respond(Default::default());
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = response.into_body().collect().await?.to_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(&body)?,
        json!({
            "status": "ok",
            "service": "daemon_http",
            "observability": {
                "dropped_storage": 0,
                "dropped_oversized": 0,
                "truncated_records": 0,
            },
        })
    );
    Ok(())
}

#[tokio::test]
async fn health_and_router_are_an_exact_allowlist() -> TestResult {
    let server = HttpTestServer::start().await?;

    let health = send_request(server.addr, "GET", "/health", &[], b"").await?;
    assert_eq!(health.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&health.body)?,
        json!({
            "status": "ok",
            "service": "daemon_http",
            "observability": {
                "dropped_storage": 0,
                "dropped_oversized": 0,
                "truncated_records": 0,
            },
        })
    );
    assert!(health
        .head
        .to_ascii_lowercase()
        .contains("content-type: application/json"));

    for (method, path) in [
        ("POST", "/health"),
        ("POST", "/files/read"),
        ("POST", "/files/write"),
        ("POST", "/files/edit"),
        ("POST", "/files/blame"),
        ("POST", "/observability/snapshot"),
        ("POST", "/observability/trace"),
        ("POST", "/observability/events"),
        ("POST", "/observability/cgroup"),
        ("POST", "/observability/layerstack"),
        ("GET", "/export/x"),
        ("POST", "/export/x"),
        ("POST", "/files/list/extra"),
        ("GET", "/anything-else"),
    ] {
        let response = send_request(server.addr, method, path, &[], b"").await?;
        assert_eq!(response.status, 404, "{method} {path}");
        assert_eq!(response.body, b"not found", "{method} {path}");
    }

    server.stop().await
}

#[tokio::test]
async fn http_connection_and_blocking_overloads_are_bounded_and_structured() -> TestResult {
    let connection_limited = HttpTestServer::start_with_admission(8, 0).await?;
    let response = send_request(connection_limited.addr, "POST", "/files/list", &[], b"{}").await?;
    assert_eq!(response.status, 503);
    let body: Value = serde_json::from_slice(&response.body)?;
    assert_eq!(body["error"]["kind"], "server_busy");
    assert_eq!(
        body["error"]["details"]["fields"]["max_concurrent_connections"],
        0
    );
    connection_limited.stop().await?;

    let blocking_limited = HttpTestServer::start_with_admission(0, 1).await?;
    let response = send_request(blocking_limited.addr, "POST", "/files/list", &[], b"{}").await?;
    assert_eq!(response.status, 429);
    let body: Value = serde_json::from_slice(&response.body)?;
    assert_eq!(body["error"]["kind"], "server_busy");
    assert_eq!(
        body["error"]["details"]["fields"]["max_blocking_requests"],
        0
    );
    blocking_limited.stop().await
}

#[tokio::test]
async fn file_list_preserves_root_published_live_and_transport_contracts() -> TestResult {
    let server = HttpTestServer::start().await?;

    let root = send_request(server.addr, "POST", "/files/list", &[], b"").await?;
    assert_eq!(root.status, 200);
    let root: Value = serde_json::from_slice(&root.body)?;
    let names = entry_names(&root);
    assert!(names.contains(&"base.txt"), "base root entry: {root}");
    assert!(
        names.contains(&"published.txt"),
        "published snapshot entry: {root}"
    );

    let bounded = send_request(
        server.addr,
        "POST",
        "/files/list",
        &[("Content-Type", "application/json")],
        br#"{"limit":1}"#,
    )
    .await?;
    assert_eq!(bounded.status, 200);
    let bounded: Value = serde_json::from_slice(&bounded.body)?;
    assert_eq!(
        bounded["entries"].as_array().expect("entries array").len(),
        1
    );
    assert_eq!(bounded["truncated"], true);

    let zero_limit = send_request(
        server.addr,
        "POST",
        "/files/list",
        &[("Content-Type", "application/json")],
        br#"{"limit":0}"#,
    )
    .await?;
    assert_eq!(zero_limit.status, 200);
    let zero_limit: Value = serde_json::from_slice(&zero_limit.body)?;
    assert_eq!(zero_limit["error"]["kind"], "invalid_request");

    let live = send_request(
        server.addr,
        "POST",
        "/files/list",
        &[("Content-Type", "application/json")],
        br#"{"workspace_session_id":"live-1"}"#,
    )
    .await?;
    assert_eq!(live.status, 200);
    let live: Value = serde_json::from_slice(&live.body)?;
    assert_eq!(entry_names(&live), vec!["live.txt"]);
    assert_eq!(live["entries"][0]["kind"], "file");
    assert_eq!(live["entries"][0]["size"], 4);

    let wrong_method = send_request(server.addr, "GET", "/files/list", &[], b"").await?;
    assert_eq!(wrong_method.status, 405);
    assert_eq!(wrong_method.body, b"use POST");

    let malformed = send_request(server.addr, "POST", "/files/list", &[], b"{").await?;
    assert_transport_error(&malformed, "bad_json")?;

    let non_object = send_request(server.addr, "POST", "/files/list", &[], b"[]").await?;
    assert_transport_error(&non_object, "invalid_request")?;

    let oversized = vec![b'x'; ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES + 1];
    let oversized = send_request(server.addr, "POST", "/files/list", &[], &oversized).await?;
    assert_transport_error(&oversized, "request_too_large")?;

    server.stop().await
}

#[tokio::test]
async fn shared_forward_preserves_request_and_maps_failures() -> TestResult {
    let server = HttpTestServer::start().await?;
    let upstream = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_port = upstream.local_addr()?.port();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let request = read_http_message(&mut stream).await?;
        stream
            .write_all(
                b"HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nX-Upstream: yes\r\nConnection: close\r\n\r\nok",
            )
            .await?;
        TestResult::Ok(request)
    });

    let path = format!("/forward/shared/{upstream_port}/deep/path?hello=world");
    let response = send_request(
        server.addr,
        "POST",
        &path,
        &[("X-Test-Header", "preserved")],
        b"payload",
    )
    .await?;
    assert_eq!(response.status, 201);
    assert_eq!(response.body, b"ok");
    assert!(response.head.contains("x-upstream: yes"));

    let captured = upstream_task.await??;
    let (head, body) = split_http_message(&captured)?;
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("POST /deep/path?hello=world HTTP/1.1\r\n"));
    assert!(lower.contains("x-test-header: preserved\r\n"));
    assert!(lower.contains("x-forwarded-host: 127.0.0.1\r\n"));
    assert!(lower.contains("x-forwarded-proto: http\r\n"));
    assert!(lower.contains(&format!(
        "x-forwarded-prefix: /forward/shared/{upstream_port}\r\n"
    )));
    assert_eq!(body, b"payload");

    for path in [
        "/forward/shared/not-a-port/",
        "/forward/shared/0/",
        "/forward/not-a-route/",
    ] {
        let response = send_request(server.addr, "GET", path, &[], b"").await?;
        assert_eq!(response.status, 400, "{path}");
    }

    let unused = TcpListener::bind("127.0.0.1:0").await?;
    let unused_port = unused.local_addr()?.port();
    drop(unused);
    let response = send_request(
        server.addr,
        "GET",
        &format!("/forward/shared/{unused_port}/"),
        &[],
        b"",
    )
    .await?;
    assert_eq!(response.status, 502);
    assert_eq!(response.body, b"target connection failed");

    server.stop().await
}

#[tokio::test]
async fn stalled_upstream_maps_to_504() -> TestResult {
    let server = HttpTestServer::start().await?;
    let upstream = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_port = upstream.local_addr()?.port();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        read_http_head(&mut stream).await?;
        std::future::pending::<()>().await;
        drop(stream);
        TestResult::Ok(())
    });

    let response = timeout(
        Duration::from_secs(3),
        send_request(
            server.addr,
            "GET",
            &format!("/forward/shared/{upstream_port}/slow"),
            &[],
            b"",
        ),
    )
    .await??;
    assert_eq!(response.status, 504);
    assert_eq!(response.body, b"target timed out");

    upstream_task.abort();
    let _ = upstream_task.await;
    server.stop().await
}

#[tokio::test]
async fn isolated_forward_and_error_vocabulary_are_exact() -> TestResult {
    let server = HttpTestServer::start().await?;

    let upstream = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_port = upstream.local_addr()?.port();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let request = read_http_message(&mut stream).await?;
        stream
            .write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nisolated",
            )
            .await?;
        TestResult::Ok(request)
    });

    let path = format!("/forward/isolated=live-1/{upstream_port}/deep?query=yes");
    let response = send_request(
        server.addr,
        "POST",
        &path,
        &[("X-Isolated-Test", "preserved")],
        b"payload",
    )
    .await?;
    assert_eq!(response.status, 202);
    assert_eq!(response.body, b"isolated");

    let captured = upstream_task.await??;
    let (head, body) = split_http_message(&captured)?;
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("POST /deep?query=yes HTTP/1.1\r\n"));
    assert!(lower.contains("x-isolated-test: preserved\r\n"));
    assert!(lower.contains(&format!(
        "x-forwarded-prefix: /forward/isolated=live-1/{upstream_port}\r\n"
    )));
    assert_eq!(body, b"payload");

    let no_ip = send_request(
        server.addr,
        "GET",
        "/forward/isolated=no-ip/3000/deep?query=yes",
        &[],
        b"",
    )
    .await?;
    assert_eq!(no_ip.status, 403);
    assert_eq!(no_ip.body, b"isolated workspace has no reachable IP");

    let unknown = send_request(
        server.addr,
        "GET",
        "/forward/isolated=missing/3000/",
        &[],
        b"",
    )
    .await?;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.body, b"unknown isolated workspace");

    server.stop().await
}

#[tokio::test]
async fn isolated_forward_rejects_a_finalize_failed_session() -> TestResult {
    let server = HttpTestServer::start().await?;
    let workspace_session_id = WorkspaceSessionId("live-1".to_owned());

    assert!(
        server
            .operations
            .workspace_session
            .guarded_destroy(workspace_session_id, None)
            .is_err(),
        "the scripted destroy failure leaves the session cleanup-only"
    );

    let response = send_request(
        server.addr,
        "GET",
        "/forward/isolated=live-1/3000/",
        &[],
        b"",
    )
    .await?;
    assert_eq!(response.status, 404);
    assert_eq!(response.body, b"unknown isolated workspace");

    server.stop().await
}

#[tokio::test]
async fn forward_upgrade_tunnels_bytes() -> TestResult {
    let server = HttpTestServer::start().await?;
    let upstream = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_port = upstream.local_addr()?.port();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        let head = read_http_head(&mut stream).await?;
        let head = String::from_utf8(head)?;
        assert!(head.starts_with("GET /socket?mode=test HTTP/1.1\r\n"));
        assert!(head.to_ascii_lowercase().contains("upgrade: eos-test\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: eos-test\r\n\r\n",
            )
            .await?;
        let mut payload = [0_u8; 4];
        stream.read_exact(&mut payload).await?;
        stream.write_all(&payload).await?;
        TestResult::Ok(())
    });

    let mut client = TcpStream::connect(server.addr).await?;
    client
        .write_all(
            format!(
                "GET /forward/shared/{upstream_port}/socket?mode=test HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: eos-test\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let head = timeout(Duration::from_secs(3), read_http_head(&mut client)).await??;
    let head = String::from_utf8(head)?;
    assert!(head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
    client.write_all(b"ping").await?;
    let mut echoed = [0_u8; 4];
    timeout(Duration::from_secs(3), client.read_exact(&mut echoed)).await??;
    assert_eq!(&echoed, b"ping");
    upstream_task.await??;

    server.stop().await
}

#[tokio::test]
async fn forward_upgrade_remains_admitted_and_shutdown_drains_tasks() -> TestResult {
    let server = HttpTestServer::start_with_admission(8, 1).await?;
    let upstream = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_port = upstream.local_addr()?.port();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await?;
        read_http_head(&mut stream).await?;
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: eos-test\r\n\r\n",
            )
            .await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        TestResult::Ok(())
    });

    let mut client = TcpStream::connect(server.addr).await?;
    client
        .write_all(
            format!(
                "GET /forward/shared/{upstream_port}/socket HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: eos-test\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let head = timeout(Duration::from_secs(1), read_http_head(&mut client)).await??;
    assert!(String::from_utf8(head)?.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert_eq!(server.connection_admission.in_use(), 1);
    assert!(server.async_tasks.len() >= 2);
    let overloaded = send_request(server.addr, "GET", "/health", &[], b"").await?;
    assert_eq!(overloaded.status, 503);

    server.stop().await?;
    timeout(Duration::from_secs(1), upstream_task).await???;
    drop(client);
    Ok(())
}

#[tokio::test]
async fn daemon_shutdown_runs_runtime_teardown_and_removes_published_artifacts() -> TestResult {
    // Keep the Unix-domain socket below the platform path-length limit; the
    // general HTTP fixture root is intentionally more descriptive and longer.
    let root = PathBuf::from(format!("/tmp/eos-daemon-life-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let operations = test_operations(&root)?;
    let config = server_config(&root);
    let socket_path = config.socket_path.clone();
    let pid_path = config.pid_path.clone();
    let shutdown = CancellationToken::new();
    let server = crate::rpc::SandboxDaemonServer {
        blocking_admission: crate::rpc::BlockingAdmission::new(config.max_blocking_requests),
        connection_admission: ConnectionAdmission::new(config.max_concurrent_connections),
        async_tasks: TaskTracker::new(),
        config,
        operations: Arc::clone(&operations),
        observability: None,
        shutdown: shutdown.clone(),
    };

    let serve = tokio::spawn(server.serve());
    timeout(Duration::from_secs(1), async {
        while !(socket_path.exists() && pid_path.exists()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon publishes socket and pid");

    shutdown.cancel();
    let result = timeout(Duration::from_secs(3), serve)
        .await
        .expect("daemon shutdown is bounded")
        .expect("serve task joins");
    let error = result.expect_err("incomplete runtime teardown is reported");
    assert!(error.to_string().contains("runtime shutdown incomplete"));
    assert!(
        !socket_path.exists(),
        "socket artifact must always be removed"
    );
    assert!(!pid_path.exists(), "pid artifact must always be removed");

    let report = operations.shutdown();
    assert!(!report.is_complete());
    assert_eq!(report.sessions_observed, 2);
    assert_eq!(report.failures.len(), 2);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

pub(crate) fn test_operations(root: &Path) -> TestResult<Arc<SandboxRuntimeOperations>> {
    let workspace_root = root.join("workspace");
    let layer_stack_root = root.join("layer-stack");
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("base.txt"), b"base")?;
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)?;
    let mut stack = LayerStack::open(layer_stack_root.clone())?;
    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse("published.txt")?,
        content: b"published".to_vec(),
    }])?;
    let manifest = stack.read_active_manifest()?;
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId("lease-http-test".to_owned()),
        manifest_version: manifest.version,
        root_hash: manifest_root_hash(&manifest),
        layer_paths: manifest
            .layers
            .iter()
            .map(|layer| layer_stack_root.join(&layer.path))
            .collect(),
        manifest,
    };
    let create_workspace_root = workspace_root.clone();
    let create_snapshot = snapshot.clone();
    let workspace_runtime = Arc::new(WorkspaceRuntimeService::from_hooks_for_test(
        WorkspaceRuntimeHooks {
            take_holder_exit_subscription: Box::new(|| Ok(None)),
            isolated_ip: Box::new(|workspace_id| {
                Ok((workspace_id.0 == "live-1").then_some(std::net::Ipv4Addr::LOCALHOST))
            }),
            holder_is_live: Box::new(WorkspaceHandle::holder_is_live),
            holder_probe: Box::new(|handle| {
                if handle.holder_is_live() {
                    HolderProbe::Running
                } else {
                    HolderProbe::Exited
                }
            }),
            holder_finalization: Box::new(|_| HolderFinalization::Exited),
            holder_exit_reason: Box::new(WorkspaceHandle::holder_exit_reason),
            allocate_workspace_session_id: Box::new(|network| {
                Ok(WorkspaceSessionId(
                    match network {
                        NetworkProfile::Isolated => "live-1",
                        NetworkProfile::Shared => "no-ip",
                    }
                    .to_owned(),
                ))
            }),
            create_workspace: Box::new(move |request: CreateWorkspaceRequest| {
                let upperdir = create_workspace_root
                    .join("upper")
                    .join(&request.workspace_session_id.0);
                let workdir = create_workspace_root
                    .join("work")
                    .join(&request.workspace_session_id.0);
                std::fs::create_dir_all(&upperdir).map_err(|error| WorkspaceError::Setup {
                    step: format!("create test upperdir: {error}"),
                })?;
                std::fs::create_dir_all(&workdir).map_err(|error| WorkspaceError::Setup {
                    step: format!("create test workdir: {error}"),
                })?;
                Ok(WorkspaceHandle::holder_backed_for_test(
                    request.workspace_session_id,
                    create_workspace_root.clone(),
                    request.network,
                    create_snapshot.clone(),
                    upperdir,
                    workdir,
                ))
            }),
            capture_changes: Box::new(
                |_handle: &WorkspaceHandle, _request: CaptureChangesRequest| {
                    Err(WorkspaceError::Capture {
                        message: "not configured".to_owned(),
                    })
                },
            ),
            capture_changes_after_holder_quiesced: Box::new(
                |_handle, _proof, _request| {
                    Err(WorkspaceError::Capture {
                        message: "not configured".to_owned(),
                    })
                },
            ),
            destroy_workspace: Box::new(
                |_handle: WorkspaceHandle, _request: DestroyWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            commit_workspace_destroy: Box::new(|_| {}),
            run_file_op: Box::new(|_handle, _op| {
                Ok(run_result_ok(&FileRunnerResult::ListDir {
                    existed: true,
                    entries: vec![FileRunnerDirEntry {
                        name: "live.txt".to_owned(),
                        kind: FileRunnerDirEntryKind::File,
                        size: Some(4),
                    }],
                    truncated: false,
                }))
            }),
            latest_snapshot: Box::new(|| {
                Err(WorkspaceError::SnapshotAcquire {
                    source: "not configured".to_owned(),
                })
            }),
        },
    ));
    let file = Arc::new(FileService::open(
        root.join("file-audit"),
        sandbox_runtime::FileRuntimeConfig::default(),
    )?);
    let layerstack = Arc::new(LayerStackService::new(
        layer_stack_root,
        root.join("scratch"),
        LayerstackRuntimeConfig::default(),
        Observer::disabled(),
        Arc::clone(&file),
    )?);
    let workspace_session = Arc::new(WorkspaceSessionService::new(
        workspace_runtime,
        Arc::clone(&layerstack),
        Observer::disabled(),
    ));
    workspace_session.create_workspace_session(CreateSessionRequest {
        network: NetworkProfile::Isolated,
        finalize_policy: FinalizePolicy::NoOp,
    })?;
    workspace_session.create_workspace_session(CreateSessionRequest {
        network: NetworkProfile::Shared,
        finalize_policy: FinalizePolicy::NoOp,
    })?;
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace_session),
        CommandConfig {
            scratch_root: root.join("commands"),
            ..CommandConfig::default()
        },
        Observer::disabled(),
    ));
    Ok(Arc::new(SandboxRuntimeOperations::new(
        command,
        workspace_session,
        layerstack,
        file,
    )))
}

fn server_config(root: &Path) -> ServerConfig {
    ServerConfig {
        socket_path: root.join("runtime.sock"),
        pid_path: root.join("runtime.pid"),
        tcp_host: None,
        tcp_port: None,
        http_host: None,
        http_port: None,
        auth_token: None,
        sandbox_id: Some("sandbox-http-test".to_owned()),
        cgroup_root: None,
        observability: ObservabilityConfig::default(),
        limits: ProtocolLimits::default(),
        max_concurrent_connections: 256,
        worker_threads: 2,
        max_blocking_requests: 8,
        blocking_thread_keep_alive_s: 5.0,
        forward: DaemonHttpForwardConfig {
            connect_timeout_s: 10.0,
            response_timeout_s: 0.1,
        },
    }
}

fn test_root(label: &str) -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "sandbox-daemon-http-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create HTTP test root");
    root
}

fn entry_names(value: &Value) -> Vec<&str> {
    value["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("entry name"))
        .collect()
}

fn assert_transport_error(response: &RawResponse, kind: &str) -> TestResult {
    assert_eq!(response.status, 400);
    let value: Value = serde_json::from_slice(&response.body)?;
    assert_eq!(value["error"]["kind"], kind);
    Ok(())
}

async fn send_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> TestResult<RawResponse> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(body).await?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let (head, body) = split_http_message(&bytes)?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status")?
        .parse()?;
    Ok(RawResponse {
        status,
        head: head.to_owned(),
        body: body.to_vec(),
    })
}

async fn read_http_message(stream: &mut TcpStream) -> TestResult<Vec<u8>> {
    let mut bytes = read_http_head(stream).await?;
    let (head, _) = split_http_message(&bytes)?;
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; content_length];
    stream.read_exact(&mut body).await?;
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

async fn read_http_head(stream: &mut TcpStream) -> TestResult<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(bytes);
        }
        if bytes.len() > 64 * 1024 {
            return Err("HTTP head exceeds test limit".into());
        }
    }
}

fn split_http_message(bytes: &[u8]) -> TestResult<(&str, &[u8])> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("missing HTTP head terminator")?;
    Ok((
        std::str::from_utf8(&bytes[..split + 4])?,
        &bytes[split + 4..],
    ))
}
