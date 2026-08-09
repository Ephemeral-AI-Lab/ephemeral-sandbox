use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use sandbox_gateway::TcpSandboxDaemonClient;
use sandbox_manager::{SandboxDaemonClient, SandboxDaemonEndpoint};
use sandbox_operation_contract::{OperationRequest, OperationResponse, OperationScope};
use sandbox_protocol::{ProtocolLimits, DAEMON_AUTH_FIELD};
use serde_json::{json, Value};

#[test]
fn successful_exchange_preserves_authenticated_newline_framing() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        let request = read_request(&mut stream);
        let response =
            sandbox_protocol::response_line(&OperationResponse::from_json_value(json!({
                "status": "ok",
                "request_id": request["request_id"],
            })));
        stream.write_all(&response).expect("write daemon response");
        request
    });

    let response = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            operation_request("request-success"),
            Some(Duration::from_secs(1)),
        )
        .expect("daemon exchange succeeds");

    assert_eq!(
        response.into_json_value(),
        json!({"status": "ok", "request_id": "request-success"})
    );
    let captured_request = server.join().expect("daemon server thread");
    assert_eq!(captured_request["op"], "run_command");
    assert_eq!(captured_request["request_id"], "request-success");
    assert_eq!(captured_request[DAEMON_AUTH_FIELD], "token");
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
            operation_request("request-timeout"),
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
fn total_timeout_bounds_drip_fed_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        read_request(&mut stream);
        stream
            .set_nodelay(true)
            .expect("disable response buffering");
        for _ in 0..12 {
            if stream.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            operation_request("request-drip-timeout"),
            Some(Duration::from_millis(30)),
        )
        .expect_err("partial response progress must not reset total timeout");

    assert_eq!(
        error.to_string(),
        "sandbox daemon forwarding failed: daemon request timed out after 30 ms"
    );
    server.join().expect("daemon server thread");
}

#[test]
fn non_newline_terminated_response_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        read_request(&mut stream);
        stream
            .write_all(br#"{"status":"ok"}"#)
            .expect("write daemon response");
    });

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            operation_request("request-non-newline"),
            Some(Duration::from_secs(1)),
        )
        .expect_err("non-newline response must fail");

    assert_eq!(
        error.to_string(),
        "sandbox daemon forwarding failed: daemon response was not newline terminated"
    );
    server.join().expect("daemon server thread");
}

#[test]
fn oversized_response_is_rejected_at_protocol_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept daemon client");
        read_request(&mut stream);
        stream
            .write_all(&vec![b'a'; ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES + 1])
            .expect("write oversized daemon response");
    });

    let error = TcpSandboxDaemonClient::new()
        .invoke(
            &SandboxDaemonEndpoint::new("127.0.0.1", port, "token"),
            operation_request("request-oversized"),
            Some(Duration::from_secs(5)),
        )
        .expect_err("oversized response must fail");

    assert_eq!(
        error.to_string(),
        format!(
            "sandbox daemon forwarding failed: daemon response exceeded {} bytes",
            ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES
        )
    );
    server.join().expect("daemon server thread");
}

#[test]
fn concurrent_calls_remain_independent() {
    const CALL_COUNT: usize = 8;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let mut workers = Vec::with_capacity(CALL_COUNT);
        for _ in 0..CALL_COUNT {
            let (mut stream, _) = listener.accept().expect("accept daemon client");
            workers.push(thread::spawn(move || {
                let request = read_request(&mut stream);
                let request_id = request["request_id"]
                    .as_str()
                    .expect("request id string")
                    .to_owned();
                let response =
                    sandbox_protocol::response_line(&OperationResponse::from_json_value(json!({
                        "request_id": request_id,
                    })));
                stream.write_all(&response).expect("write daemon response");
                request
            }));
        }
        workers
            .into_iter()
            .map(|worker| worker.join().expect("daemon worker thread"))
            .collect::<Vec<_>>()
    });
    let client = Arc::new(TcpSandboxDaemonClient::new());
    let endpoint = Arc::new(SandboxDaemonEndpoint::new("127.0.0.1", port, "token"));
    let barrier = Arc::new(Barrier::new(CALL_COUNT + 1));
    let callers = (0..CALL_COUNT)
        .map(|index| {
            let client = Arc::clone(&client);
            let endpoint = Arc::clone(&endpoint);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let request_id = format!("request-concurrent-{index}");
                barrier.wait();
                let response = client
                    .invoke(
                        &endpoint,
                        operation_request(&request_id),
                        Some(Duration::from_secs(2)),
                    )
                    .expect("concurrent daemon exchange succeeds");
                assert_eq!(response.into_json_value()["request_id"], request_id);
            })
        })
        .collect::<Vec<_>>();

    barrier.wait();
    for caller in callers {
        caller.join().expect("daemon caller thread");
    }
    let captured_requests = server.join().expect("daemon server thread");
    assert_eq!(captured_requests.len(), CALL_COUNT);
    assert!(captured_requests
        .iter()
        .all(|request| request[DAEMON_AUTH_FIELD] == "token"));
}

fn operation_request(request_id: &str) -> OperationRequest {
    OperationRequest::new(
        "run_command",
        request_id,
        OperationScope::sandbox("sandbox-1"),
        json!({}),
    )
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut line = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut line)
        .expect("read daemon request");
    assert!(line.ends_with(b"\n"));
    serde_json::from_slice(&line).expect("decode daemon request")
}
