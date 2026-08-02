use sandbox_operation_client::{GatewayClient, GatewayClientError, MAX_REQUEST_BYTES};
use sandbox_operation_contract::{OperationRequest, OperationScope};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn gateway(response: Vec<u8>) -> (String, tokio::task::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake gateway");
    let addr = listener.local_addr().expect("gateway address").to_string();
    let worker = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept client");
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .await
            .expect("read request");
        stream.write_all(&response).await.expect("write response");
        serde_json::from_slice(&request).expect("request JSON")
    });
    (addr, worker)
}

fn request(args: Value) -> OperationRequest {
    OperationRequest::new(
        "list_sandboxes",
        "request-1",
        OperationScope::system(),
        args,
    )
}

#[tokio::test]
async fn send_adds_transport_fields_and_returns_json() {
    let (addr, worker) = gateway(b"{\"ok\":true}\n".to_vec()).await;
    let client = GatewayClient::new(addr, Some("secret".to_owned()));
    let response = client.send(&request(json!({}))).await.expect("response");
    let received = worker.await.expect("gateway task");

    assert_eq!(response, json!({"ok": true}));
    assert_eq!(received["_sandbox_gateway_auth_token"], "secret");
    assert_eq!(received["_stream_logs"], false);
    assert_eq!(received["op"], "list_sandboxes");
}

#[tokio::test]
async fn send_with_logs_delivers_each_log_before_the_response() {
    let response = b"cli_log(\"starting\")\ncli_log(\"ready\")\n{\"ok\":true}\n".to_vec();
    let (addr, worker) = gateway(response).await;
    let client = GatewayClient::new(addr, None);
    let mut logs = Vec::new();
    let response = client
        .send_with_logs(&request(json!({})), true, |line| logs.push(line.to_owned()))
        .await
        .expect("streamed response");
    worker.await.expect("gateway task");

    assert_eq!(logs, ["starting", "ready"]);
    assert_eq!(response, json!({"ok": true}));
}

#[tokio::test]
async fn oversized_encoded_request_is_rejected_before_connection() {
    assert_eq!(
        MAX_REQUEST_BYTES,
        sandbox_protocol::ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES
    );
    let client = GatewayClient::new("127.0.0.1:1", None);
    let error = client
        .send(&request(json!({"content": "x".repeat(MAX_REQUEST_BYTES)})))
        .await
        .expect_err("oversized request");

    assert!(matches!(&error, GatewayClientError::Protocol(_)));
    assert_eq!(
        error.to_string(),
        format!("gateway request exceeded {MAX_REQUEST_BYTES} bytes")
    );
}

#[tokio::test]
async fn explicit_tcp_endpoint_uses_the_shared_transport() {
    let (addr, worker) = gateway(b"{\"transport\":\"tcp\"}\n".to_vec()).await;
    let client = GatewayClient::new(format!("tcp://{addr}"), None);
    let response = client.send(&request(json!({}))).await.expect("response");
    worker.await.expect("gateway task");

    assert_eq!(response, json!({"transport": "tcp"}));
}

#[tokio::test]
async fn legacy_dns_tcp_endpoint_remains_supported() {
    let (addr, worker) = gateway(b"{\"transport\":\"tcp-dns\"}\n".to_vec()).await;
    let port = addr
        .parse::<std::net::SocketAddr>()
        .expect("fake gateway address")
        .port();
    let client = GatewayClient::new(format!("localhost:{port}"), None);
    let response = client.send(&request(json!({}))).await.expect("response");
    worker.await.expect("gateway task");

    assert_eq!(response, json!({"transport": "tcp-dns"}));
}

#[tokio::test]
async fn invalid_endpoint_fails_before_transport_connection() {
    let client = GatewayClient::new("http://127.0.0.1:7878", None);
    let error = client
        .send(&request(json!({})))
        .await
        .expect_err("invalid endpoint");

    assert!(matches!(&error, GatewayClientError::Endpoint(_)));
    assert_eq!(error.kind(), "connection_error");
    assert_eq!(
        error.to_string(),
        "invalid gateway endpoint: unsupported gateway endpoint scheme"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn named_pipe_endpoint_round_trips_a_gateway_request() {
    use sandbox_operation_client::GatewayEndpoint;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_path = format!(
        r"\\.\pipe\sandbox-operation-client-{}",
        uuid::Uuid::new_v4()
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_path)
        .expect("create named pipe");
    let worker = tokio::spawn(async move {
        server.connect().await.expect("connect named-pipe client");
        let mut reader = BufReader::new(server);
        let mut request = Vec::new();
        reader
            .read_until(b'\n', &mut request)
            .await
            .expect("read request");
        reader
            .get_mut()
            .write_all(b"{\"transport\":\"npipe\"}\n")
            .await
            .expect("write response");
        serde_json::from_slice::<Value>(&request).expect("request JSON")
    });
    let endpoint = GatewayEndpoint::windows_named_pipe(pipe_path).expect("named-pipe endpoint");
    let client = GatewayClient::from_endpoint(endpoint, None);
    let response = client.send(&request(json!({}))).await.expect("response");
    let received = worker.await.expect("gateway task");

    assert_eq!(response, json!({"transport": "npipe"}));
    assert_eq!(received["op"], "list_sandboxes");
}

#[cfg(windows)]
#[tokio::test]
async fn named_pipe_endpoint_handles_concurrency_five_bursts() {
    use sandbox_operation_client::GatewayEndpoint;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    const CLIENTS: usize = 5;

    let pipe_path = format!(
        r"\\.\pipe\sandbox-operation-client-burst-{}",
        uuid::Uuid::new_v4()
    );
    let first_server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_path)
        .expect("create first named-pipe instance");
    let mut servers = vec![first_server];
    for _ in 1..CLIENTS {
        servers.push(
            ServerOptions::new()
                .create(&pipe_path)
                .expect("create pending named-pipe instance"),
        );
    }
    let worker = tokio::spawn(async move {
        let mut handlers = Vec::with_capacity(CLIENTS);
        for server in servers {
            handlers.push(tokio::spawn(async move {
                server.connect().await.expect("connect named-pipe client");
                let mut reader = BufReader::new(server);
                let mut request = Vec::new();
                reader
                    .read_until(b'\n', &mut request)
                    .await
                    .expect("read request");
                reader
                    .get_mut()
                    .write_all(b"{\"transport\":\"npipe\"}\n")
                    .await
                    .expect("write response");
            }));
        }
        for handler in handlers {
            handler.await.expect("named-pipe handler");
        }
    });
    let endpoint = GatewayEndpoint::windows_named_pipe(pipe_path).expect("named-pipe endpoint");
    let mut clients = Vec::new();
    for _ in 0..CLIENTS {
        let client = GatewayClient::from_endpoint(endpoint.clone(), None);
        clients.push(tokio::spawn(async move {
            client.send(&request(json!({}))).await.expect("response")
        }));
    }
    for client in clients {
        assert_eq!(
            client.await.expect("client task"),
            json!({"transport": "npipe"})
        );
    }
    worker.await.expect("gateway task");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_endpoint_round_trips_a_gateway_request() {
    use sandbox_operation_client::GatewayEndpoint;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    let socket_path = std::env::temp_dir().join(format!(
        "sandbox-operation-client-{}.sock",
        uuid::Uuid::new_v4()
    ));
    let listener = UnixListener::bind(&socket_path).expect("bind Unix socket");
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept client");
        let mut reader = BufReader::new(stream);
        let mut request = Vec::new();
        reader
            .read_until(b'\n', &mut request)
            .await
            .expect("read request");
        reader
            .get_mut()
            .write_all(b"{\"transport\":\"unix\"}\n")
            .await
            .expect("write response");
        serde_json::from_slice::<Value>(&request).expect("request JSON")
    });
    let endpoint = GatewayEndpoint::unix_socket(&socket_path).expect("Unix endpoint");
    let client = GatewayClient::from_endpoint(endpoint, None);
    let response = client.send(&request(json!({}))).await.expect("response");
    let received = worker.await.expect("gateway task");
    std::fs::remove_file(&socket_path).expect("remove Unix socket");

    assert_eq!(response, json!({"transport": "unix"}));
    assert_eq!(received["op"], "list_sandboxes");
}
