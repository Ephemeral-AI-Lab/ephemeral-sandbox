#![cfg(feature = "manager")]

mod support;

use sandbox_cli::manager::run_cli_with_writers;
use serde_json::{json, Value};
use support::{fake_gateway, help_operation_names, parse_json_line};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

async fn fake_gateway_raw(response: Vec<u8>) -> (String, JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake gateway");
    let addr = listener.local_addr().expect("fake gateway address");
    let request = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept CLI connection");
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read)
            .read_line(&mut line)
            .await
            .expect("read CLI request");
        let request = serde_json::from_str(&line).expect("request JSON line");
        write
            .write_all(&response)
            .await
            .expect("write raw gateway response");
        request
    });
    (addr.to_string(), request)
}

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
async fn help_lists_exact_management_catalog() {
    let (code, stdout, stderr) = run(&["sandbox-manager-cli", "help"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, include_str!("fixtures/manager-help.txt"));
    assert_eq!(
        help_operation_names(&stdout),
        [
            "create_sandbox",
            "destroy_sandbox",
            "list_sandboxes",
            "inspect_sandbox",
            "squash_layerstacks",
            "export_changes",
        ]
    );
    assert!(stdout.contains("Use:\n  sandbox-manager-cli OPERATION"));
}

#[tokio::test]
async fn operation_help_uses_manager_program_name() {
    let (code, stdout, stderr) = run(&["sandbox-manager-cli", "help", "create_sandbox"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Usage\n  sandbox-manager-cli create_sandbox"));
    assert!(stdout.contains("--image string required"));
    assert!(stdout.contains("--count integer optional"));
    assert!(stdout.contains("Default: 1"));
    assert!(stdout.contains(
        "Examples\n  sandbox-manager-cli create_sandbox --image ubuntu:24.04 \
--workspace-bind-root /testbed"
    ));
}

#[tokio::test]
async fn bare_invocation_prints_manager_catalog_help() {
    let (code, stdout, stderr) = run(&["sandbox-manager-cli"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Sandbox Manager Help"));
    assert!(stdout.contains("Use:\n  sandbox-manager-cli OPERATION"));
}

#[tokio::test]
async fn squash_help_uses_plural_public_operation_name() {
    let (code, stdout, _) = run(&["sandbox-manager-cli", "help", "squash_layerstacks"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage\n  sandbox-manager-cli squash_layerstacks"));
}

#[tokio::test]
async fn manager_rejects_operations_from_other_sets_and_old_names() {
    for operation in [
        "checkpoint_squash",
        "exec_command",
        "snapshot",
        "observability",
    ] {
        let (code, stdout, stderr) = run(&["sandbox-manager-cli", operation]).await;
        assert_eq!(code, 2, "{operation}");
        assert!(stdout.is_empty(), "{operation}");
        let error = parse_json_line(&stderr);
        assert_eq!(error["error"]["kind"], "invalid_request");
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("error message")
                .contains(&format!("unknown operation: {operation}")),
            "{error}"
        );
    }
}

#[tokio::test]
async fn missing_and_invalid_operation_arguments_are_json_usage_errors() {
    let (code, stdout, stderr) = run(&["sandbox-manager-cli", "create_sandbox"]).await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("--image is required for create_sandbox"));

    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "create_sandbox",
        "--image",
        "ubuntu:24.04",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert!(parse_json_line(&stderr)["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("--workspace-bind-root is required for create_sandbox"));

    let (code, _, stderr) = run(&["sandbox-manager-cli", "list_sandboxes", "extra"]).await;
    assert_eq!(code, 2);
    let error = parse_json_line(&stderr);
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("unexpected positional argument"));
}

#[tokio::test]
async fn omitted_create_count_uses_catalog_default() {
    let response = json!({"id": "eos-created"});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "create_sandbox",
        "--image",
        "ubuntu:24.04",
        "--workspace-bind-root",
        "/workspace",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(
        request["args"],
        json!({
            "image": "ubuntu:24.04",
            "workspace_root": "/workspace",
            "count": 1
        })
    );
}

#[tokio::test]
async fn progress_streams_to_stderr_and_keeps_final_json_on_stdout() {
    let response = json!({"sandboxes": []});
    let raw_response = format!("cli_log(\"manager fixture progress\")\n{response}\n").into_bytes();
    let (addr, received) = fake_gateway_raw(raw_response).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "--progress",
        "list_sandboxes",
    ])
    .await;

    assert_eq!(code, 0);
    assert_eq!(parse_json_line(&stdout), response);
    assert!(stderr.starts_with("[progress "));
    assert!(stderr.contains("manager fixture progress\n"));
    assert!(stderr.ends_with("[Output]\n"));
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["_stream_logs"], true);
}

#[tokio::test]
async fn create_accepts_legacy_operation_progress_flag() {
    let response = json!({"id": "eos-created"});
    let raw_response = format!("cli_log(\"create fixture progress\")\n{response}\n").into_bytes();
    let (addr, received) = fake_gateway_raw(raw_response).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "create_sandbox",
        "--image",
        "ubuntu:24.04",
        "--workspace-bind-root",
        "/workspace",
        "--progress",
    ])
    .await;

    assert_eq!(code, 0);
    assert_eq!(parse_json_line(&stdout), response);
    assert!(stderr.contains("create fixture progress\n"));
    assert!(stderr.ends_with("[Output]\n"));
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["_stream_logs"], true);
    assert_eq!(request["args"]["count"], 1);
}

#[tokio::test]
async fn parser_and_config_failures_are_json_usage_errors() {
    let (code, stdout, stderr) = run(&["sandbox-manager-cli", "--gateway-socket"]).await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "invalid_request");

    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-auth-token",
        "",
        "list_sandboxes",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "config_error");
}

#[tokio::test]
async fn success_is_one_stdout_json_line_and_uses_system_scope() {
    let response = json!({"sandboxes": []});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "list_sandboxes",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "list_sandboxes");
    assert_eq!(request["scope"], json!({"kind": "system"}));
    assert_eq!(request["args"], json!({}));
    assert_eq!(request["_stream_logs"], false);
    assert!(request["request_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[tokio::test]
async fn gateway_operation_failure_is_one_unchanged_stderr_json_line() {
    let response = json!({
        "error": {
            "kind": "operation_failed",
            "message": "manager refused",
            "details": {"reason": "fixture"}
        }
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "list_sandboxes",
    ])
    .await;

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr), response);
    received.await.expect("fake gateway task");
}

#[tokio::test]
async fn malformed_gateway_response_is_protocol_error_on_stderr() {
    let (addr, received) = fake_gateway_raw(b"not-json\n".to_vec()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-manager-cli",
        "--gateway-socket",
        &addr,
        "list_sandboxes",
    ])
    .await;

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "protocol_error");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("gateway response json failed"));
    assert_eq!(error["error"]["details"], json!({}));
    received.await.expect("fake gateway task");
}

#[tokio::test]
async fn help_command_errors_are_json_usage_errors() {
    for args in [
        vec!["sandbox-manager-cli", "help", "unknown"],
        vec!["sandbox-manager-cli", "help", "list_sandboxes", "extra"],
    ] {
        let (code, stdout, stderr) = run(&args).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(parse_json_line(&stderr)["error"]["kind"], "invalid_request");
    }
}
