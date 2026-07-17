use std::time::Duration;

use http::StatusCode;
use sandbox_operation_catalog::internal::runtime::EXPORT_LAYERSTACK;
use sandbox_operation_catalog::observability::{
    CGROUP_SPEC, EVENTS_SPEC, LAYERSTACK_SPEC, SNAPSHOT_SPEC, TRACE_SPEC,
};
use sandbox_operation_client::MAX_REQUEST_BYTES;
use serde_json::json;

use crate::support;

const GATEWAY_AUTH_FIELD: &str = "_sandbox_gateway_auth_token";
const COMPATIBILITY_LIST_RESPONSE: &str = r#"{"sandboxes":[]}"#;

fn rpc_body(op: &str) -> serde_json::Value {
    json!({
        "op": op,
        "scope": {"kind": "system"},
        "args": {},
    })
}

#[tokio::test]
async fn one_shot_injects_credentials_server_side_and_passes_result_through() {
    let expected: serde_json::Value =
        serde_json::from_str(COMPATIBILITY_LIST_RESPONSE).expect("compatibility list fixture");
    let gateway = support::FakeGateway::spawn({
        let expected = expected.clone();
        move |_| vec![expected.to_string()]
    })
    .await;
    let console = support::spawn_console_default(gateway.addr).await;

    let mut request = rpc_body("list_sandboxes");
    request[GATEWAY_AUTH_FIELD] = json!("browser-supplied-token");
    request["_stream_logs"] = json!(true);
    let response = support::post_rpc(console, &request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .values()
            .all(|value| !String::from_utf8_lossy(value.as_bytes())
                .contains(support::TEST_AUTH_TOKEN))
    );
    let body = support::body_json(response).await;
    assert_eq!(body, expected);
    let rendered = body.to_string();
    assert!(!rendered.contains(support::TEST_AUTH_TOKEN));
    assert!(!rendered.contains("browser-supplied-token"));

    let seen = gateway.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["op"], "list_sandboxes");
    assert_eq!(seen[0]["scope"], json!({"kind": "system"}));
    assert_eq!(seen[0][GATEWAY_AUTH_FIELD], support::TEST_AUTH_TOKEN);
    assert_eq!(seen[0]["_stream_logs"], json!(false));
    assert!(
        seen[0]["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "console must inject a request_id"
    );
}

#[tokio::test]
async fn runtime_file_call_uses_authenticated_gateway_rpc() {
    let gateway = support::FakeGateway::spawn(|_| {
        vec![json!({
            "path": "src/lib.rs",
            "content": "pub fn run() {}",
            "start_line": 1,
            "num_lines": 1,
            "total_lines": 1,
            "bytes_read": 15,
            "total_bytes": 15,
            "next_offset": null,
            "truncated": false,
        })
        .to_string()]
    })
    .await;
    let console = support::spawn_console_default(gateway.addr).await;
    let body = json!({
        "op": "file_read",
        "scope": {"kind": "sandbox", "sandbox_id": "eos-files"},
        "args": {"path": "src/lib.rs", "offset": 1, "limit": 20},
    });

    let response = support::post_rpc(console, &body).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(support::body_json(response).await["path"], "src/lib.rs");
    let seen = gateway.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["op"], "file_read");
    assert_eq!(
        seen[0]["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-files"})
    );
    assert_eq!(
        seen[0]["args"],
        json!({"path": "src/lib.rs", "offset": 1, "limit": 20})
    );
    assert_eq!(seen[0][GATEWAY_AUTH_FIELD], support::TEST_AUTH_TOKEN);
}

#[tokio::test]
async fn concrete_observability_calls_preserve_the_rpc_envelope() {
    let gateway =
        support::FakeGateway::spawn(|_| vec![json!({"accepted": true}).to_string()]).await;
    let console = support::spawn_console_default(gateway.addr).await;
    let expected = [
        (SNAPSHOT_SPEC.name, json!({})),
        (TRACE_SPEC.name, json!({"trace_id": "last"})),
        (EVENTS_SPEC.name, json!({"last_n": 10})),
        (
            CGROUP_SPEC.name,
            json!({"scope": "sandbox", "window_ms": 30000}),
        ),
        (LAYERSTACK_SPEC.name, json!({"window_ms": 30000})),
    ];
    for (operation, args) in &expected {
        let body = json!({
            "op": operation,
            "scope": {"kind": "sandbox", "sandbox_id": "eos-observe"},
            "args": args,
        });
        let response = support::post_rpc(console, &body).await;
        assert_eq!(response.status(), StatusCode::OK, "{operation}");
        assert_eq!(support::body_json(response).await["accepted"], true);
    }

    let seen = gateway.requests();
    assert_eq!(seen.len(), expected.len());
    for (request, (operation, args)) in seen.iter().zip(expected) {
        assert_eq!(request["op"], operation);
        assert_eq!(
            request["scope"],
            json!({"kind": "sandbox", "sandbox_id": "eos-observe"})
        );
        assert_eq!(request["args"], args);
        assert!(request["args"].get("view").is_none());
        assert_eq!(request[GATEWAY_AUTH_FIELD], support::TEST_AUTH_TOKEN);
    }
}

#[tokio::test]
async fn protocol_error_returns_in_body_with_http_200() {
    let gateway = support::FakeGateway::spawn(|_| {
        vec![json!({
            "error": {"kind": "invalid_request", "message": "boom", "details": {}}
        })
        .to_string()]
    })
    .await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc(console, &rpc_body("inspect_sandbox")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "invalid_request");
    assert_eq!(body["error"]["message"], "boom");
}

#[tokio::test]
async fn unknown_operation_is_rejected_before_gateway_transport() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc(console, &rpc_body("phase0_unknown_operation")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "invalid_request");
    assert_eq!(body["error"]["details"], json!({}));
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("phase0_unknown_operation")));
    assert_eq!(gateway.request_count(), 0);
}

#[tokio::test]
async fn internal_operation_is_rejected_before_gateway_transport() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;
    let body = json!({
        "op": EXPORT_LAYERSTACK,
        "scope": {"kind": "sandbox", "sandbox_id": "eos-internal"},
        "args": {},
    });

    let response = support::post_rpc(console, &body).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "invalid_request");
    assert_eq!(body["error"]["details"], json!({}));
    assert_eq!(gateway.request_count(), 0);
}

#[tokio::test]
async fn public_operation_with_undeclared_scope_is_rejected_before_gateway_transport() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc(console, &rpc_body("file_read")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        support::body_json(response).await["error"]["kind"],
        "invalid_request"
    );
    assert_eq!(gateway.request_count(), 0);
}

#[tokio::test]
async fn sandbox_observability_operation_with_system_scope_is_rejected_before_gateway_transport() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc(console, &rpc_body(TRACE_SPEC.name)).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        support::body_json(response).await["error"]["kind"],
        "invalid_request"
    );
    assert_eq!(gateway.request_count(), 0);
}

#[tokio::test]
async fn sandbox_scope_and_client_request_id_pass_through() {
    let gateway = support::FakeGateway::spawn(|_| vec![json!({"ok": true}).to_string()]).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let body = json!({
        "op": "exec_command",
        "request_id": "req-fixed-1",
        "scope": {"kind": "sandbox", "sandbox_id": "eos-abc"},
        "args": {"cmd": "pwd", "yield_time_ms": 0},
    });
    let response = support::post_rpc(console, &body).await;

    assert_eq!(response.status(), StatusCode::OK);
    let seen = gateway.requests();
    assert_eq!(seen[0]["request_id"], "req-fixed-1");
    assert_eq!(
        seen[0]["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-abc"})
    );
    assert_eq!(seen[0]["args"], json!({"cmd": "pwd", "yield_time_ms": 0}));
}

#[tokio::test]
async fn malformed_body_is_400() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc_raw(console, "this is not json", None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "bad_json");

    let response = support::post_rpc(console, &json!({"scope": {"kind": "system"}})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "invalid_request");

    assert_eq!(
        gateway.request_count(),
        0,
        "bad bodies never reach the gateway"
    );
}

#[tokio::test]
async fn body_over_client_limit_is_rejected_before_gateway_transport() {
    assert_eq!(MAX_REQUEST_BYTES, 16 * 1024 * 1024);
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;
    let body = json!({
        "op": "file_write",
        "scope": {"kind": "sandbox", "sandbox_id": "eos-limit"},
        "args": {"content": "x".repeat(MAX_REQUEST_BYTES)},
    })
    .to_string();

    let response = support::post_rpc_raw(console, &body, None).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        support::body_json(response).await,
        json!({
            "error": {
                "kind": "request_too_large",
                "message": "request body exceeded the protocol size limit",
                "details": {}
            }
        })
    );
    assert_eq!(gateway.request_count(), 0);
}

#[tokio::test]
async fn gateway_unreachable_is_502() {
    let closed = support::closed_port().await;
    let console = support::spawn_console_default(closed).await;

    let response = support::post_rpc(console, &rpc_body("list_sandboxes")).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "connection_error");
}

#[tokio::test]
async fn gateway_timeout_is_504() {
    let gateway = support::FakeGateway::spawn_silent().await;
    let console = support::spawn_console(gateway.addr, None, Duration::from_millis(200)).await;

    let response = support::post_rpc(console, &rpc_body("list_sandboxes")).await;

    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "gateway_timeout");
}

#[tokio::test]
async fn sse_variant_streams_logs_then_result() {
    let gateway = support::FakeGateway::spawn(|_| {
        vec![
            "cli_log(\"creating sandbox eos-1...\")".to_owned(),
            "cli_log(\"daemon ready\")".to_owned(),
            json!({"id": "eos-1", "state": "ready"}).to_string(),
        ]
    })
    .await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc_sse(console, &rpc_body("create_sandbox")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/event-stream"));

    let text = support::body_text(response).await;
    let events: Vec<&str> = text.split("\n\n").filter(|part| !part.is_empty()).collect();
    assert_eq!(events.len(), 3, "two logs plus one result: {text}");
    assert_eq!(
        events[0],
        "event: log\ndata: {\"line\":\"creating sandbox eos-1...\"}"
    );
    assert_eq!(events[1], "event: log\ndata: {\"line\":\"daemon ready\"}");
    assert!(events[2].starts_with("event: result\ndata: "));

    let seen = gateway.requests();
    assert_eq!(seen[0]["_stream_logs"], json!(true));
}

#[tokio::test]
async fn sse_transport_failure_emits_error_event() {
    let gateway = support::FakeGateway::spawn(|_| vec!["cli_log(\"starting\")".to_owned()]).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::post_rpc_sse(console, &rpc_body("create_sandbox")).await;
    let text = support::body_text(response).await;

    assert!(text.contains("event: log"));
    assert!(
        text.contains("event: error"),
        "stream ends in an error event: {text}"
    );
}
