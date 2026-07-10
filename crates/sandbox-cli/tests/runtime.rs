#![cfg(feature = "runtime")]

mod support;

use std::time::Duration;

use sandbox_cli::runtime::run_cli_with_writers;
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
async fn help_lists_exact_runtime_catalog() {
    let (code, stdout, stderr) =
        run(&["sandbox-runtime-cli", "--sandbox-id", "eos-x", "help"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(
        help_operation_names(&stdout),
        [
            "exec_command",
            "write_command_stdin",
            "read_command_lines",
            "file_read",
            "file_write",
            "file_edit",
            "file_blame",
        ]
    );
    assert!(stdout.contains("Use:\n  sandbox-runtime-cli --sandbox-id ID OPERATION"));
}

#[tokio::test]
async fn operation_help_uses_runtime_program_name() {
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "help",
        "exec_command",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID exec_command \
[--workspace-session-id ID] [--timeout-ms N] [--yield-time-ms N] COMMAND"
    ));
    assert!(stdout.contains("COMMAND string required"));
    assert!(stdout.contains("Examples\n  sandbox-runtime-cli --sandbox-id ID exec_command pwd"));

    let (code, stdout, stderr) = run(&["sandbox-runtime-cli", "help", "write_command_stdin"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID write_command_stdin \
--command-session-id ID [--yield-time-ms N] TEXT"
    ));

    let (code, stdout, stderr) = run(&["sandbox-runtime-cli", "help", "file_read"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Usage\n  sandbox-runtime-cli --sandbox-id ID file_read"));
    assert!(stdout.contains("Default: 1"));
    assert!(stdout.contains("Default: 2000"));

    let (code, stdout, stderr) = run(&["sandbox-runtime-cli", "help", "read_command_lines"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Default: 0"));
    assert!(stdout.contains("Default: 200"));
}

#[tokio::test]
async fn bare_invocation_prints_runtime_catalog_help() {
    let (code, stdout, stderr) = run(&["sandbox-runtime-cli"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Sandbox Runtime Help"));
    assert!(stdout.contains("Use:\n  sandbox-runtime-cli --sandbox-id ID OPERATION"));
}

#[tokio::test]
async fn missing_or_empty_sandbox_id_fails_before_gateway_io() {
    for sandbox_id in [None, Some(""), Some("   ")] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind unreachable gateway");
        let addr = listener.local_addr().expect("gateway address").to_string();
        let mut args = vec!["sandbox-runtime-cli", "--gateway-socket", &addr];
        if let Some(sandbox_id) = sandbox_id {
            args.extend(["--sandbox-id", sandbox_id]);
        }
        args.extend(["exec_command", "pwd"]);
        let (code, stdout, stderr) = run(&args).await;

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(parse_json_line(&stderr)["error"]["kind"], "invalid_request");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "runtime usage error connected to the gateway"
        );
    }
}

#[tokio::test]
async fn runtime_rejects_other_set_and_internal_operations() {
    for operation in [
        "list_sandboxes",
        "snapshot",
        "squash_layerstack",
        "file_list",
        "create_workspace_session",
        "destroy_workspace_session",
    ] {
        let (code, stdout, stderr) =
            run(&["sandbox-runtime-cli", "--sandbox-id", "eos-x", operation]).await;
        assert_eq!(code, 2, "{operation}");
        assert!(stdout.is_empty(), "{operation}");
        let error = parse_json_line(&stderr);
        assert!(error["error"]["message"]
            .as_str()
            .expect("error message")
            .contains(&format!("unknown operation: {operation}")));
    }
}

#[tokio::test]
async fn invalid_operation_arguments_are_json_usage_errors() {
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "exec_command",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("COMMAND is required for exec_command"));

    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "exec_command",
        "--shell",
        "bash",
        "pwd",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(parse_json_line(&stderr)["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("unknown flag for exec_command: --shell"));
}

#[tokio::test]
async fn parser_and_config_failures_are_json_usage_errors() {
    let (code, stdout, stderr) = run(&["sandbox-runtime-cli", "--gateway-socket"]).await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "invalid_request");

    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-auth-token",
        "",
        "--sandbox-id",
        "eos-x",
        "exec_command",
        "pwd",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr)["error"]["kind"], "config_error");
}

#[tokio::test]
async fn success_is_one_stdout_json_line_and_uses_sandbox_scope() {
    let response = json!({"status": "exited", "exit_code": 0});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "exec_command",
        "pwd",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "exec_command");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(request["args"], json!({"cmd": "pwd"}));
    assert_eq!(request["_stream_logs"], false);
}

#[tokio::test]
async fn omitted_read_arguments_use_catalog_defaults() {
    let response = json!({"status": "running"});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "read_command_lines",
        "--command-session-id",
        "cmd-1",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(
        request["args"],
        json!({"command_session_id": "cmd-1", "start_offset": 0, "limit": 200})
    );

    let response = json!({"path": "README.md"});
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "file_read",
        "--path",
        "README.md",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(
        request["args"],
        json!({"path": "README.md", "offset": 1, "limit": 2000})
    );
}

#[tokio::test]
async fn gateway_operation_failure_is_one_unchanged_stderr_json_line() {
    let response = json!({
        "error": {
            "kind": "command_failed",
            "message": "runtime refused",
            "details": {"exit_code": 17}
        }
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "exec_command",
        "pwd",
    ])
    .await;

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert_eq!(parse_json_line(&stderr), response);
    received.await.expect("fake gateway task");
}
