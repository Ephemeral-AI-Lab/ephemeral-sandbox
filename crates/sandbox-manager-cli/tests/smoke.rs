//! Local (no-gateway) smoke matrix mirroring the operation reference: help
//! surfaces, per-binary usage program names, and local usage errors (exit 2).

use sandbox_manager_cli::run_cli_with_writers;

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
async fn bare_prints_manager_catalog_help() {
    let (code, stdout, _) = run(&["sandbox-manager-cli"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Sandbox Manager Help"));
    assert!(stdout.contains("Use:\n  sandbox-manager-cli OPERATION"));
}

#[tokio::test]
async fn operation_help_uses_manager_program_name() {
    let (code, stdout, _) = run(&["sandbox-manager-cli", "help", "create_sandbox"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage\n  sandbox-manager-cli create_sandbox"));
    assert!(!stdout.contains("sandbox-cli manager"));
}

#[tokio::test]
async fn observability_bare_prints_observability_catalog_help() {
    let (code, stdout, _) = run(&["sandbox-manager-cli", "observability"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Sandbox Observability Help"));
    assert!(stdout.contains("Use:\n  sandbox-manager-cli observability OPERATION"));
}

#[tokio::test]
async fn observability_operation_help_uses_subcommand_program_name() {
    let (code, stdout, _) = run(&["sandbox-manager-cli", "observability", "help", "trace"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage\n  sandbox-manager-cli observability trace"));
}

#[tokio::test]
async fn unknown_operation_is_local_usage_error() {
    let (code, _, stderr) = run(&["sandbox-manager-cli", "frobnicate"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains(r#""kind":"invalid_request""#));
    assert!(stderr.contains("unknown operation: frobnicate"));
}

#[tokio::test]
async fn runtime_operation_typed_here_is_unknown() {
    let (code, _, stderr) = run(&["sandbox-manager-cli", "exec_command", "pwd"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown operation: exec_command"));
}

#[tokio::test]
async fn hidden_snapshot_op_is_not_in_manager_catalog() {
    let (code, _, stderr) = run(&["sandbox-manager-cli", "snapshot"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown operation: snapshot"));
}

#[tokio::test]
async fn create_sandbox_missing_required_arg_is_usage_error() {
    let (code, _, stderr) = run(&[
        "sandbox-manager-cli",
        "create_sandbox",
        "--image",
        "ubuntu:24.04",
    ])
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("--workspace-bind-root is required for create_sandbox"));
}

#[tokio::test]
async fn list_sandboxes_rejects_stray_positional() {
    let (code, _, stderr) = run(&["sandbox-manager-cli", "list_sandboxes", "foo"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unexpected positional argument for list_sandboxes: foo"));
}

#[tokio::test]
async fn observability_op_rejects_stray_positional() {
    let (code, _, stderr) = run(&[
        "sandbox-manager-cli",
        "observability",
        "snapshot",
        "--sandbox-id",
        "eos-x",
        "extra",
    ])
    .await;
    // "extra" is an unexpected positional for an op with no positional args.
    assert_eq!(code, 2);
    assert!(stderr.contains("unexpected positional argument"));
}
