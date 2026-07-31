use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use sandbox_gateway::TcpSandboxDaemonClient;
use sandbox_manager::{SandboxDaemonClient, SandboxDaemonEndpoint};
use sandbox_operation_contract::{OperationRequest, OperationResponse, OperationScope};
use serde_json::json;

fn request() -> OperationRequest {
    OperationRequest::new(
        "run_command",
        "request-1",
        OperationScope::sandbox("sandbox-1"),
        json!({}),
    )
}

fn spawn_daemon(response: Vec<u8>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        let mut request_line = String::new();
        BufReader::new(stream.try_clone().expect("clone daemon client connection"))
            .read_line(&mut request_line)
            .expect("read authenticated request");
        assert!(request_line.ends_with('\n'));
        stream.write_all(&response).expect("write daemon response");
    });
    (port, server)
}

#[test]
fn blocking_exchange_preserves_authenticated_round_trip() {
    let expected = OperationResponse::ok(json!({"status": "ok"}));
    let (port, server) = spawn_daemon(sandbox_protocol::response_line(&expected));

    let response = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_secs(1)),
        )
        .expect("blocking daemon exchange");

    assert_eq!(response.as_json_value(), expected.as_json_value());
    server.join().expect("daemon server thread");
}

#[test]
fn blocking_exchange_is_safe_inside_tokio_spawn_blocking() {
    let expected = OperationResponse::ok(json!({"status": "spawn-blocking"}));
    let (port, server) = spawn_daemon(sandbox_protocol::response_line(&expected));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let response = runtime
        .block_on(async move {
            tokio::task::spawn_blocking(move || {
                TcpSandboxDaemonClient::new().invoke(
                    &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
                    request(),
                    Some(Duration::from_secs(1)),
                )
            })
            .await
        })
        .expect("spawn blocking task")
        .expect("blocking daemon exchange");

    assert_eq!(response.as_json_value(), expected.as_json_value());
    server.join().expect("daemon server thread");
}

#[test]
fn explicit_timeout_bounds_stalled_daemon_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept daemon client");
        thread::sleep(Duration::from_millis(100));
    });
    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_millis(20)),
        )
        .expect_err("stalled daemon response must time out");

    assert_eq!(
        error.to_string(),
        "sandbox daemon forwarding failed: daemon request timed out after 20 ms"
    );
    server.join().expect("daemon server thread");
}

#[test]
fn explicit_timeout_is_a_whole_exchange_deadline_for_trickled_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        let mut request_line = String::new();
        BufReader::new(stream.try_clone().expect("clone daemon client connection"))
            .read_line(&mut request_line)
            .expect("read authenticated request");
        let response = sandbox_protocol::response_line(&OperationResponse::ok(json!({
            "status": "a deliberately long trickled response"
        })));
        for byte in response {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
    });
    let started = Instant::now();
    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_millis(50)),
        )
        .expect_err("trickled daemon response must hit the whole-exchange deadline");
    let elapsed = started.elapsed();

    assert_eq!(
        error.to_string(),
        "sandbox daemon forwarding failed: daemon request timed out after 50 ms"
    );
    assert!(
        elapsed < Duration::from_millis(300),
        "whole-exchange timeout took {elapsed:?}"
    );
    server.join().expect("daemon server thread");
}

#[test]
fn malformed_response_fails_closed() {
    let (port, server) = spawn_daemon(b"{not-json}\n".to_vec());

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_secs(1)),
        )
        .expect_err("malformed response must fail");

    assert!(error.to_string().contains("decode daemon response failed"));
    server.join().expect("daemon server thread");
}

#[test]
fn unterminated_response_fails_closed() {
    let response = sandbox_protocol::response_line(&OperationResponse::ok(json!({})));
    let (port, server) = spawn_daemon(response[..response.len() - 1].to_vec());

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_secs(1)),
        )
        .expect_err("unterminated response must fail");

    assert!(error
        .to_string()
        .contains("daemon response was not newline terminated"));
    server.join().expect("daemon server thread");
}

#[test]
fn oversized_response_fails_closed() {
    let mut response = vec![b'x'; sandbox_protocol::ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES + 1];
    response.push(b'\n');
    let (port, server) = spawn_daemon(response);

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            request(),
            Some(Duration::from_secs(5)),
        )
        .expect_err("oversized response must fail");

    assert!(
        error.to_string().contains("daemon response exceeded"),
        "unexpected oversized-response error: {error}"
    );
    server.join().expect("daemon server thread");
}
