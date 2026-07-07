//! Local (no-gateway) smoke matrix mirroring the operation reference: help
//! surfaces, the required `--sandbox-id`, and local usage errors (exit 2).

use sandbox_runtime_cli::run_cli_with_writers;

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
async fn bare_prints_runtime_catalog_help_with_sandbox_id_usage() {
    let (code, stdout, _) = run(&["sandbox-runtime-cli"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Sandbox Runtime Help"));
    assert!(stdout.contains("Use:\n  sandbox-runtime-cli --sandbox-id ID OPERATION"));
}

#[tokio::test]
async fn operation_help_uses_runtime_program_name() {
    let (code, stdout, _) = run(&["sandbox-runtime-cli", "help", "exec_command"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage\n  sandbox-runtime-cli --sandbox-id ID exec_command"));
    assert!(!stdout.contains("sandbox-cli runtime"));
}

#[tokio::test]
async fn missing_sandbox_id_is_usage_error() {
    let (code, _, stderr) = run(&["sandbox-runtime-cli", "exec_command", "pwd"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains(r#""kind":"invalid_request""#));
    assert!(stderr.contains("runtime operations require --sandbox-id"));
}

#[tokio::test]
async fn empty_sandbox_id_is_usage_error() {
    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "",
        "exec_command",
        "pwd",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("runtime sandbox id must be non-empty"));
}

#[tokio::test]
async fn manager_operation_typed_here_is_unknown() {
    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "list_sandboxes",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown operation: list_sandboxes"));
}

#[tokio::test]
async fn exec_command_unknown_flag_is_usage_error() {
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
    assert!(stderr.contains("unknown flag for exec_command: --shell"));
}

#[tokio::test]
async fn exec_command_missing_positional_is_usage_error() {
    // A flag with no positional COMMAND: argv is non-empty so this dispatches to
    // request building, which reports the missing required positional.
    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "exec_command",
        "--timeout-ms",
        "100",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("COMMAND is required for exec_command"));
}

#[tokio::test]
async fn hidden_squash_layerstack_op_is_not_in_runtime_catalog() {
    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "squash_layerstack",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown operation: squash_layerstack"));
}

#[tokio::test]
async fn file_list_is_not_in_runtime_catalog() {
    let (code, _, stderr) =
        run(&["sandbox-runtime-cli", "--sandbox-id", "eos-x", "file_list"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown operation: file_list"));
}

#[tokio::test]
async fn file_read_stays_in_runtime_catalog() {
    let (code, stdout, _) = run(&["sandbox-runtime-cli", "help", "file_read"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage\n  sandbox-runtime-cli --sandbox-id ID file_read"));
}

#[tokio::test]
async fn destroy_workspace_session_rejects_non_float_grace() {
    let (code, _, stderr) = run(&[
        "sandbox-runtime-cli",
        "--sandbox-id",
        "eos-x",
        "destroy_workspace_session",
        "--workspace-session-id",
        "ws-1",
        "--grace-s",
        "abc",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("--grace-s must be a finite number"));
}
