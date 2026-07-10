#![cfg(feature = "observability")]

mod support;

use std::time::Duration;

use sandbox_cli::observability::run_cli_with_writers;
use serde_json::json;
use support::{fake_gateway, help_operation_names, parse_json_line};
use tokio::net::TcpListener;

async fn run(args: &[&str]) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_cli_with_writers(args.iter().copied(), &mut stdout, &mut stderr).await;
    (
        code,
        String::from_utf8(stdout).expect("stdout utf8"),
        String::from_utf8(stderr).expect("stderr utf8"),
    )
}

#[tokio::test]
async fn help_lists_exact_observability_catalog() {
    let (code, stdout, stderr) = run(&["sandbox-observability-cli", "help"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, include_str!("fixtures/observability-help.txt"));
    assert_eq!(
        help_operation_names(&stdout),
        ["snapshot", "trace", "events", "cgroup", "layerstack"]
    );
    assert!(stdout.contains("Use:\n  sandbox-observability-cli OPERATION"));
}

#[tokio::test]
async fn operation_help_uses_standalone_program_name() {
    let (code, stdout, stderr) = run(&["sandbox-observability-cli", "help", "trace"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Usage\n  sandbox-observability-cli trace"));
    assert!(!stdout.contains("sandbox-manager-cli observability"));
    assert!(stdout.contains("--sandbox-id string required"));
    assert!(stdout.contains("--trace-id string optional"));
    assert!(stdout.contains("Default: last"));
    assert!(stdout.contains(
        "Examples\n  sandbox-observability-cli trace --sandbox-id eos-abc \
--trace-id req-7f3"
    ));
}

#[tokio::test]
async fn aggregate_snapshot_uses_system_scope() {
    let response = json!({"sandboxes": []});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-observability-cli",
        "--gateway-socket",
        &addr,
        "snapshot",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "snapshot");
    assert_eq!(request["scope"], json!({"kind": "system"}));
    assert_eq!(request["args"], json!({}));
    assert_eq!(request["_stream_logs"], false);
}

#[tokio::test]
async fn scoped_views_use_hidden_observability_operation_and_catalog_defaults() {
    let cases = [
        ("snapshot", json!({"view": "snapshot"})),
        ("trace", json!({"view": "trace", "trace_id": "last"})),
        ("events", json!({"view": "events"})),
        (
            "cgroup",
            json!({"view": "cgroup", "scope": "sandbox", "window_ms": 60000}),
        ),
        (
            "layerstack",
            json!({"view": "layerstack", "window_ms": 60000}),
        ),
    ];

    for (view, expected_args) in cases {
        let response = json!({"view": view});
        let (addr, received) = fake_gateway(response.clone()).await;
        let (code, stdout, stderr) = run(&[
            "sandbox-observability-cli",
            "--gateway-socket",
            &addr,
            view,
            "--sandbox-id",
            "eos-x",
        ])
        .await;

        assert_eq!(code, 0, "{view}");
        assert!(stderr.is_empty(), "{view}: {stderr}");
        assert_eq!(parse_json_line(&stdout), response);
        let request = received.await.expect("fake gateway task");
        assert_eq!(request["op"], "get_observability", "{view}");
        assert_eq!(
            request["scope"],
            json!({"kind": "sandbox", "sandbox_id": "eos-x"}),
            "{view}"
        );
        assert_eq!(request["args"], expected_args, "{view}");
        assert_eq!(request["_stream_logs"], false, "{view}");
    }
}

#[tokio::test]
async fn non_snapshot_views_require_sandbox_id_before_gateway_io() {
    for view in ["trace", "events", "cgroup", "layerstack"] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind unreachable gateway");
        let addr = listener.local_addr().expect("gateway address").to_string();
        let (code, stdout, stderr) =
            run(&["sandbox-observability-cli", "--gateway-socket", &addr, view]).await;

        assert_eq!(code, 2, "{view}");
        assert!(stdout.is_empty(), "{view}");
        let error = parse_json_line(&stderr);
        assert_eq!(error["error"]["kind"], "invalid_request", "{view}");
        let message = error["error"]["message"].as_str().expect("error message");
        assert!(message.contains("--sandbox-id"), "{message}");
        assert!(message.contains("is required"), "{message}");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "{view} usage error connected to the gateway"
        );
    }
}

#[tokio::test]
async fn empty_sandbox_id_is_a_json_usage_error_before_gateway_io() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unreachable gateway");
    let addr = listener.local_addr().expect("gateway address").to_string();
    let (code, stdout, stderr) = run(&[
        "sandbox-observability-cli",
        "--gateway-socket",
        &addr,
        "snapshot",
        "--sandbox-id",
        "",
    ])
    .await;

    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("--sandbox-id must be non-empty"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn observability_rejects_management_and_runtime_operations() {
    for operation in ["list_sandboxes", "exec_command", "file_list"] {
        let (code, stdout, stderr) = run(&["sandbox-observability-cli", operation]).await;
        assert_eq!(code, 2, "{operation}");
        assert!(stdout.is_empty(), "{operation}");
        assert!(parse_json_line(&stderr)["error"]["message"]
            .as_str()
            .expect("error message")
            .contains(&format!("unknown operation: {operation}")));
    }
}

#[tokio::test]
async fn parser_and_config_failures_are_json_usage_errors() {
    let (code, stdout, stderr) = run(&["sandbox-observability-cli", "--gateway-socket"]).await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "invalid_request");

    let (code, stdout, stderr) = run(&[
        "sandbox-observability-cli",
        "--gateway-auth-token",
        "",
        "snapshot",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "config_error");
}

#[tokio::test]
async fn stray_positional_argument_fails_before_gateway_io() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unreachable gateway");
    let addr = listener.local_addr().expect("gateway address").to_string();
    let (code, stdout, stderr) = run(&[
        "sandbox-observability-cli",
        "--gateway-socket",
        &addr,
        "snapshot",
        "extra",
    ])
    .await;

    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("unexpected positional argument"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn gateway_operation_failure_is_one_unchanged_stderr_json_line() {
    let response = json!({
        "error": {
            "kind": "observability_failed",
            "message": "sampling failed",
            "details": {"view": "snapshot", "retryable": true}
        }
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-observability-cli",
        "--gateway-socket",
        &addr,
        "snapshot",
    ])
    .await;

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr), response);
    received.await.expect("fake gateway task");
}
