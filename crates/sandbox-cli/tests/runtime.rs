#![cfg(feature = "runtime")]

mod support;

use std::io::Cursor;
use std::time::Duration;

use sandbox_cli::runtime::{run_cli_with_streams, run_cli_with_writers};
use serde_json::json;
use support::{fake_gateway, help_operation_names, parse_json_line};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    assert_eq!(stdout, include_str!("fixtures/runtime-help.txt"));
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
            "create_workspace_session",
            "create_mpla_workspace_session",
            "attach_mpla_prepared_fixture",
            "activate_workspace_session",
            "fork_workspace_session",
            "rollback_workspace_session",
            "publish_mpla_workspace_session",
            "squash_mpla_branch",
            "publish_workspace_session",
            "destroy_workspace_session",
            "mpla_storage_admin",
        ]
    );
    assert!(stdout
        .contains("Use:\n  sandbox-runtime-cli --sandbox-id ID [--request-id VALUE] OPERATION"));
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

    let (code, stdout, stderr) =
        run(&["sandbox-runtime-cli", "help", "publish_workspace_session"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID publish_workspace_session \
--workspace-session-id ID [--grace-s SECONDS]"
    ));
    assert!(stdout.contains(
        "Capture the unpublished changes of an explicit workspace session, merge them safely into the current LayerStack when possible, and close the session. Rejected or failed pre-commit publishes retain the session."
    ));
    assert!(stdout.contains("--workspace-session-id string required"));
    assert!(stdout.contains("--grace-s float optional"));

    let (code, stdout, stderr) =
        run(&["sandbox-runtime-cli", "help", "rollback_workspace_session"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID --request-id OPERATION_ID \
rollback_workspace_session --run-id RUN_ID --branch BRANCH --target-branch TARGET"
    ));
    assert!(stdout.contains("--run-id string required"));
    assert!(stdout.contains("--branch string required"));
    assert!(stdout.contains("--target-branch string required"));

    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "help",
        "attach_mpla_prepared_fixture",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID --request-id OPERATION_ID attach_mpla_prepared_fixture --run-id RUN_ID --fixture-profile PROFILE"
    ));
    assert!(stdout.contains("--run-id string required"));
    assert!(stdout.contains("--fixture-profile string required"));

    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "help",
        "publish_mpla_workspace_session",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID --request-id OPERATION_ID \
publish_mpla_workspace_session --workspace-session-id ID --branch BRANCH"
    ));
    assert!(stdout.contains("--workspace-session-id string required"));
    assert!(stdout.contains("--branch string required"));

    let (code, stdout, stderr) = run(&["sandbox-runtime-cli", "help", "squash_mpla_branch"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains(
        "Usage\n  sandbox-runtime-cli --sandbox-id ID --request-id OPERATION_ID \
squash_mpla_branch --run-id RUN_ID --branch BRANCH"
    ));
    assert!(stdout.contains("--run-id string required"));
    assert!(stdout.contains("--branch string required"));
}

#[tokio::test]
async fn bare_invocation_prints_runtime_catalog_help() {
    let (code, stdout, stderr) = run(&["sandbox-runtime-cli"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Sandbox Runtime Help"));
    assert!(stdout
        .contains("Use:\n  sandbox-runtime-cli --sandbox-id ID [--request-id VALUE] OPERATION"));
}

#[tokio::test]
async fn request_id_defaults_to_uuid_v4() {
    let mut request_ids = Vec::new();

    for _ in 0..2 {
        let response = json!({"status": "exited", "exit_code": 0});
        let (addr, received) = fake_gateway(response).await;
        let (code, _stdout, stderr) = run(&[
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
        let request = received.await.expect("fake gateway task");
        request_ids.push(
            request["request_id"]
                .as_str()
                .expect("request id string")
                .to_owned(),
        );
    }

    for request_id in &request_ids {
        let parsed = uuid::Uuid::parse_str(request_id).expect("request id UUID");
        assert_eq!(parsed.get_version_num(), 4);
    }
    assert_ne!(request_ids[0], request_ids[1]);
}

#[tokio::test]
async fn explicit_request_id_is_forwarded_unchanged() {
    let response = json!({"status": "exited", "exit_code": 0});
    let (addr, received) = fake_gateway(response).await;
    let (code, _stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "-demo-run_01:A01.001-1",
        "exec_command",
        "pwd",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["request_id"], "-demo-run_01:A01.001-1");
}

#[tokio::test]
async fn duplicate_request_id_is_rejected() {
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "first",
        "--request-id",
        "second",
        "exec_command",
        "pwd",
    ])
    .await;

    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    let error = parse_json_line(&stderr);
    assert_eq!(error["error"]["kind"], "invalid_request");
    assert!(error["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("cannot be used multiple times"));
}

#[tokio::test]
async fn request_id_accepts_length_boundaries_and_rejects_invalid_values() {
    for valid in ["a".to_owned(), "Z9._:-".repeat(21) + "Z9"] {
        assert_eq!(valid.len(), if valid == "a" { 1 } else { 128 });
        let response = json!({"status": "exited", "exit_code": 0});
        let (addr, received) = fake_gateway(response).await;
        let (code, _stdout, stderr) = run(&[
            "sandbox-runtime-cli",
            "--gateway-socket",
            &addr,
            "--sandbox-id",
            "eos-x",
            "--request-id",
            &valid,
            "exec_command",
            "pwd",
        ])
        .await;
        assert_eq!(code, 0, "valid request id {valid:?}");
        assert!(stderr.is_empty());
        let request = received.await.expect("fake gateway task");
        assert_eq!(request["request_id"], valid);
    }

    let too_long = "a".repeat(129);
    let mut invalid_values = vec![String::new(), too_long, "café".to_owned(), "🛒".to_owned()];
    invalid_values.extend(
        (0_u8..=127)
            .filter(|byte| !(byte.is_ascii_alphanumeric() || b"._:-".contains(byte)))
            .map(|byte| String::from_utf8(vec![b'a', byte, b'b']).expect("ASCII test value")),
    );
    for invalid in &invalid_values {
        let (code, stdout, stderr) = run(&[
            "sandbox-runtime-cli",
            "--sandbox-id",
            "eos-x",
            "--request-id",
            invalid,
            "exec_command",
            "pwd",
        ])
        .await;
        assert_eq!(code, 2, "invalid request id {invalid:?}");
        assert!(stdout.is_empty());
        let error = parse_json_line(&stderr);
        assert_eq!(error["error"]["kind"], "invalid_request");
        assert_eq!(
            error["error"]["message"],
            "--request-id must be 1-128 ASCII letters, digits, period, underscore, colon, or dash"
        );
    }
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
async fn publish_workspace_session_forwards_exact_sandbox_scoped_arguments() {
    let response = json!({
        "workspace_session_id": "ws-1",
        "publish": {
            "no_op": true,
            "revision": {
                "manifest_version": 4,
                "root_hash": "root-4",
                "layer_count": 3
            },
            "route_summary": {"source_count": 0, "ignored_count": 0}
        },
        "destroyed": true,
        "evicted_upperdir_bytes": 0
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "publish_workspace_session",
        "--workspace-session-id",
        "ws-1",
        "--grace-s",
        "1.25",
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "publish_workspace_session");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({"workspace_session_id": "ws-1", "grace_s": 1.25})
    );
}

#[tokio::test]
async fn mpla_lifecycle_operations_forward_only_sandbox_scoped_identifiers() {
    let response = json!({
        "run_id": "scorecard-run",
        "fixture_profile": "s4-chain-v1",
        "service_elapsed_ns": 123
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "attach-1",
        "attach_mpla_prepared_fixture",
        "--run-id",
        "scorecard-run",
        "--fixture-profile",
        "s4-chain-v1",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "attach_mpla_prepared_fixture");
    assert_eq!(request["request_id"], "attach-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({"run_id": "scorecard-run", "fixture_profile": "s4-chain-v1"})
    );

    let response = json!({
        "workspace_session_id": "ws-activate",
        "run_id": "scorecard-run",
        "branch": "main",
        "service_elapsed_ns": 1234
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "activate-1",
        "activate_workspace_session",
        "--run-id",
        "scorecard-run",
        "--branch",
        "main",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "activate_workspace_session");
    assert_eq!(request["request_id"], "activate-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({"run_id": "scorecard-run", "branch": "main"})
    );

    let response = json!({
        "run_id": "scorecard-run",
        "source_branch": "main",
        "branch": "agent-1",
        "service_elapsed_ns": 456
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "fork-1",
        "fork_workspace_session",
        "--run-id",
        "scorecard-run",
        "--source-branch",
        "main",
        "--branch",
        "agent-1",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "fork_workspace_session");
    assert_eq!(request["request_id"], "fork-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({
            "run_id": "scorecard-run",
            "source_branch": "main",
            "branch": "agent-1"
        })
    );

    let response = json!({
        "workspace_session_id": "ws-rollback",
        "run_id": "scorecard-run",
        "branch": "main",
        "target_branch": "checkpoint-1",
        "service_elapsed_ns": 789
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "rollback-1",
        "rollback_workspace_session",
        "--run-id",
        "scorecard-run",
        "--branch",
        "main",
        "--target-branch",
        "checkpoint-1",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "rollback_workspace_session");
    assert_eq!(request["request_id"], "rollback-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({
            "run_id": "scorecard-run",
            "branch": "main",
            "target_branch": "checkpoint-1"
        })
    );

    let response = json!({
        "workspace_session_id": "ws-publish",
        "run_id": "scorecard-run",
        "branch": "main",
        "service_elapsed_ns": 987
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "publish-1",
        "publish_mpla_workspace_session",
        "--workspace-session-id",
        "ws-publish",
        "--branch",
        "main",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "publish_mpla_workspace_session");
    assert_eq!(request["request_id"], "publish-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({"workspace_session_id": "ws-publish", "branch": "main"})
    );

    let response = json!({
        "run_id": "scorecard-run",
        "branch": "main",
        "service_elapsed_ns": 321
    });
    let (addr, received) = fake_gateway(response.clone()).await;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "squash-1",
        "squash_mpla_branch",
        "--run-id",
        "scorecard-run",
        "--branch",
        "main",
    ])
    .await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "squash_mpla_branch");
    assert_eq!(request["request_id"], "squash-1");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(
        request["args"],
        json!({"run_id": "scorecard-run", "branch": "main"})
    );
}

#[tokio::test]
async fn mpla_storage_admin_forwards_the_exact_request_json() {
    let response = json!({"status": "accepted"});
    let (addr, received) = fake_gateway(response.clone()).await;
    let request_json = r#"{"schema_version":1,"interface_version":"m2r-iface-v1"}"#;
    let (code, stdout, stderr) = run(&[
        "sandbox-runtime-cli",
        "--gateway-socket",
        &addr,
        "--sandbox-id",
        "eos-x",
        "--request-id",
        "m2r-storage-01",
        "mpla_storage_admin",
        request_json,
    ])
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(parse_json_line(&stdout), response);
    let request = received.await.expect("fake gateway task");
    assert_eq!(request["op"], "mpla_storage_admin");
    assert_eq!(request["request_id"], "m2r-storage-01");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "eos-x"})
    );
    assert_eq!(request["args"], json!({"request_json": request_json}));
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

#[tokio::test]
async fn batch_jsonl_reuses_one_sandbox_pinned_cli_and_preserves_response_streams() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind batch fake gateway");
    let addr = listener
        .local_addr()
        .expect("batch gateway address")
        .to_string();
    let received = tokio::spawn(async move {
        let responses = [
            json!({"run_id": "batch-run", "branch": "agent-1"}),
            json!({
                "error": {
                    "kind": "conflict",
                    "message": "duplicate branch",
                    "details": {}
                }
            }),
        ];
        let mut requests = Vec::<serde_json::Value>::new();
        for response in responses {
            let (stream, _) = listener.accept().await.expect("accept batch request");
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read)
                .read_line(&mut line)
                .await
                .expect("read batch request");
            requests.push(serde_json::from_str(&line).expect("batch request JSON"));
            let mut response_line = serde_json::to_vec(&response).expect("batch response JSON");
            response_line.push(b'\n');
            write
                .write_all(&response_line)
                .await
                .expect("write batch response");
        }
        requests
    });
    let input = [
        json!({
            "schema_version": 1,
            "request_id": "fork-1",
            "operation": "fork_workspace_session",
            "operation_argv": [
                "--run-id", "batch-run",
                "--source-branch", "main",
                "--branch", "agent-1"
            ]
        }),
        json!({
            "schema_version": 1,
            "request_id": "invalid request id",
            "operation": "fork_workspace_session",
            "operation_argv": []
        }),
        json!({
            "schema_version": 1,
            "request_id": "fork-2",
            "operation": "fork_workspace_session",
            "operation_argv": [
                "--run-id", "batch-run",
                "--source-branch", "main",
                "--branch", "agent-1"
            ]
        }),
    ]
    .into_iter()
    .map(|request| serde_json::to_string(&request).expect("serialize batch request"))
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_cli_with_streams(
        [
            "sandbox-runtime-cli",
            "--gateway-socket",
            &addr,
            "--gateway-auth-token",
            "batch-token",
            "--sandbox-id",
            "eos-batch",
            "--batch-jsonl",
        ],
        Cursor::new(input),
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let output = String::from_utf8(stdout).expect("batch stdout UTF-8");
    let lines = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("batch output JSON"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert_eq!(
        lines[0],
        json!({"kind": "sandbox_runtime_cli_batch_ready_v1"})
    );
    assert_eq!(lines[1]["kind"], "sandbox_runtime_cli_batch_response_v1");
    assert_eq!(lines[1]["exit_code"], 0);
    assert_eq!(lines[1]["stderr"], "");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            lines[1]["stdout"].as_str().expect("success stdout")
        )
        .expect("success response JSON"),
        json!({"run_id": "batch-run", "branch": "agent-1"})
    );
    assert_eq!(lines[2]["exit_code"], 2);
    assert_eq!(lines[2]["stdout"], "");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            lines[2]["stderr"].as_str().expect("validation stderr")
        )
        .expect("validation error JSON")["error"]["message"],
        "--request-id must be 1-128 ASCII letters, digits, period, underscore, colon, or dash"
    );
    assert_eq!(lines[3]["exit_code"], 1);
    assert_eq!(lines[3]["stdout"], "");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            lines[3]["stderr"].as_str().expect("operation stderr")
        )
        .expect("operation error JSON")["error"]["kind"],
        "conflict"
    );

    let requests = received.await.expect("batch gateway task");
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request["scope"],
            json!({"kind": "sandbox", "sandbox_id": "eos-batch"})
        );
    }
    assert_eq!(requests[0]["request_id"], "fork-1");
    assert_eq!(requests[1]["request_id"], "fork-2");
}

#[tokio::test]
async fn batch_jsonl_rejects_ambiguous_cli_scope_before_gateway_io() {
    for args in [
        vec![
            "sandbox-runtime-cli",
            "--sandbox-id",
            "eos-x",
            "--batch-jsonl",
            "exec_command",
        ],
        vec![
            "sandbox-runtime-cli",
            "--sandbox-id",
            "eos-x",
            "--batch-jsonl",
            "--request-id",
            "outside-batch",
        ],
    ] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_cli_with_streams(
            args,
            Cursor::new(Vec::<u8>::new()),
            &mut stdout,
            &mut stderr,
        )
        .await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(
            parse_json_line(&String::from_utf8(stderr).expect("batch stderr"))["error"]["message"],
            "--batch-jsonl cannot be combined with --request-id or an operation"
        );
    }
}
