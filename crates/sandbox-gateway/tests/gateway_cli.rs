use std::ffi::OsString;
use std::path::PathBuf;

use sandbox_gateway::cli::client::GatewayClient;
use sandbox_gateway::cli::config::{
    GatewayConfig, GatewayConfigOverrides, DEFAULT_GATEWAY_SOCKET, SANDBOX_DEFAULT_ID_ENV,
    SANDBOX_GATEWAY_SOCKET_ENV,
};
use sandbox_gateway::cli::output::render_response;
use sandbox_gateway::cli::request_builder::{
    build_request_from_catalog_with_id, manager_catalog_document, observability_catalog_document,
    runtime_catalog_document, BuildRequestInput, RequestBuildError,
};
use sandbox_protocol::{
    CliOperationCatalogDocument, CliOperationExecutionSpace, CliOperationScope, Request,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn manager_operation_uses_system_scope() -> TestResult {
    let request = build_manager_request("list_sandboxes", &[])?;

    assert_eq!(request.scope, CliOperationScope::System);
    assert_eq!(request.args, json!({}));
    Ok(())
}

#[test]
fn runtime_operation_requires_sandbox_without_default() -> TestResult {
    let catalog = runtime_catalog()?;
    let config = config(None);
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Runtime,
            operation: "exec_command".to_owned(),
            operation_argv: vec!["pwd".to_owned()],
            sandbox_id: None,
        },
        &config,
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("runtime request unexpectedly succeeded")?;

    assert!(error.message().contains("runtime operations require"));
    Ok(())
}

#[test]
fn runtime_operation_uses_default_sandbox_when_configured() -> TestResult {
    let request = build_runtime_request(None, &["pwd"])?;

    assert_eq!(
        request.scope,
        CliOperationScope::Sandbox {
            sandbox_id: "default-sbox".to_owned()
        }
    );
    Ok(())
}

#[test]
fn runtime_sandbox_id_populates_sandbox_scope() -> TestResult {
    let request =
        build_runtime_request(Some("sbox-1"), &["--workspace-session-id", "ws-1", "pwd"])?;

    assert_eq!(
        request.scope,
        CliOperationScope::Sandbox {
            sandbox_id: "sbox-1".to_owned()
        }
    );
    Ok(())
}

#[test]
fn runtime_sandbox_id_remains_scope_selection_not_request_arg() -> TestResult {
    let request =
        build_runtime_request(Some("sbox-1"), &["--workspace-session-id", "ws-1", "pwd"])?;

    assert_eq!(
        request.args,
        json!({
            "workspace_session_id": "ws-1",
            "cmd": "pwd",
        })
    );
    assert!(request.args.get("sandbox_id").is_none());
    Ok(())
}

#[test]
fn manager_request_construction_rejects_runtime_catalog() -> TestResult {
    let catalog = runtime_catalog()?;
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Manager,
            operation: "exec_command".to_owned(),
            operation_argv: vec![],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("manager request unexpectedly accepted runtime CLI catalog")?;

    assert_eq!(
        error.message(),
        "loaded catalog is for runtime, not manager"
    );
    Ok(())
}

#[test]
fn runtime_request_construction_rejects_manager_cli_catalog() -> TestResult {
    let catalog = manager_catalog()?;
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Runtime,
            operation: "create_sandbox".to_owned(),
            operation_argv: vec![],
            sandbox_id: Some("sbox-1".to_owned()),
        },
        &config(None),
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("runtime request unexpectedly accepted manager CLI catalog")?;

    assert_eq!(
        error.message(),
        "loaded catalog is for manager, not runtime"
    );
    Ok(())
}

#[test]
fn manager_execution_space_uses_system_scope_for_create_sandbox() -> TestResult {
    let request = build_manager_request(
        "create_sandbox",
        &["--image", "ubuntu:24.04", "--workspace-root", "/testbed"],
    )?;

    assert_eq!(request.scope, CliOperationScope::System);
    Ok(())
}

#[test]
fn create_sandbox_maps_image_and_workspace_root_args() -> TestResult {
    let request = build_manager_request(
        "create_sandbox",
        &["--image", "ubuntu:24.04", "--workspace-root", "/testbed"],
    )?;

    assert_eq!(
        request.args,
        json!({ "image": "ubuntu:24.04", "workspace_root": "/testbed" })
    );
    Ok(())
}

#[test]
fn exec_command_maps_workspace_session_id_and_command() -> TestResult {
    let request =
        build_runtime_request(Some("sbox-1"), &["--workspace-session-id", "ws-1", "pwd"])?;

    assert_eq!(request.op, "exec_command");
    assert_eq!(
        request.args,
        json!({
            "workspace_session_id": "ws-1",
            "cmd": "pwd",
        })
    );
    Ok(())
}

#[test]
fn exec_command_maps_command_without_workspace_session_id() -> TestResult {
    let request = build_runtime_request(Some("sbox-1"), &["pwd"])?;

    assert_eq!(request.op, "exec_command");
    assert_eq!(
        request.args,
        json!({
            "cmd": "pwd",
        })
    );
    Ok(())
}

#[test]
fn create_workspace_session_maps_no_profile_to_empty_args() -> TestResult {
    let request = build_runtime_operation_request("create_workspace_session", Some("sbox-1"), &[])?;

    assert_eq!(request.op, "create_workspace_session");
    assert_eq!(request.args, json!({}));
    Ok(())
}

#[test]
fn create_workspace_session_maps_network_profile_flag() -> TestResult {
    let request = build_runtime_operation_request(
        "create_workspace_session",
        Some("sbox-1"),
        &["--network-profile", "isolated"],
    )?;

    assert_eq!(request.op, "create_workspace_session");
    assert_eq!(request.args, json!({ "network_profile": "isolated" }));
    Ok(())
}

#[test]
fn destroy_workspace_session_maps_workspace_session_id_and_grace() -> TestResult {
    let request = build_runtime_operation_request(
        "destroy_workspace_session",
        Some("sbox-1"),
        &["--workspace-session-id", "ws-1", "--grace-s", "2.5"],
    )?;

    assert_eq!(request.op, "destroy_workspace_session");
    assert_eq!(
        request.args,
        json!({
            "workspace_session_id": "ws-1",
            "grace_s": 2.5,
        })
    );
    assert!(request.args.get("sandbox_id").is_none());
    Ok(())
}

#[test]
fn destroy_workspace_session_rejects_non_finite_grace() -> TestResult {
    for value in ["NaN", "inf"] {
        let error = build_runtime_operation_request(
            "destroy_workspace_session",
            Some("sbox-1"),
            &["--workspace-session-id", "ws-1", "--grace-s", value],
        )
        .err()
        .ok_or("non-finite grace unexpectedly built a request")?;
        let message = error.to_string();

        assert!(message.contains("--grace-s"), "{value}: {message}");
        assert!(message.contains("finite"), "{value}: {message}");
    }
    Ok(())
}

#[test]
fn read_command_lines_maps_command_session_id_start_offset_and_limit_flags() -> TestResult {
    let catalog = runtime_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Runtime,
            operation: "read_command_lines".to_owned(),
            operation_argv: vec![
                "--command-session-id".to_owned(),
                "cmd-1".to_owned(),
                "--start-offset".to_owned(),
                "10".to_owned(),
                "--limit".to_owned(),
                "50".to_owned(),
            ],
            sandbox_id: Some("sbox-1".to_owned()),
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    assert_eq!(
        request.args,
        json!({
            "command_session_id": "cmd-1",
            "start_offset": 10,
            "limit": 50,
        })
    );
    Ok(())
}

#[test]
fn read_command_lines_omits_default_window_args_when_flags_are_absent() -> TestResult {
    let catalog = runtime_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Runtime,
            operation: "read_command_lines".to_owned(),
            operation_argv: vec!["--command-session-id".to_owned(), "cmd-1".to_owned()],
            sandbox_id: Some("sbox-1".to_owned()),
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    assert_eq!(
        request.args,
        json!({
            "command_session_id": "cmd-1",
        })
    );
    Ok(())
}

#[test]
fn output_writes_success_to_stdout() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = render_response(&json!({ "ok": true }), &mut stdout, &mut stderr)?;

    assert_eq!(exit, 0);
    assert_eq!(String::from_utf8(stdout)?, "{\"ok\":true}\n");
    assert!(stderr.is_empty());
    Ok(())
}

#[test]
fn output_writes_errors_to_stderr() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let response = json!({
        "error": {
            "kind": "operation_failed",
            "message": "failed",
            "details": {},
        }
    });
    let exit = render_response(&response, &mut stdout, &mut stderr)?;

    assert_eq!(exit, 1);
    assert!(stdout.is_empty());
    let stderr_json = serde_json::from_slice::<Value>(&stderr)?;
    assert_eq!(stderr_json, response);
    Ok(())
}

#[test]
fn cli_files_do_not_import_gateway_server_or_runtime_internals() {
    let main = include_str!("../src/cli/main.rs");
    let client = include_str!("../src/cli/client.rs");
    let config = include_str!("../src/cli/config.rs");
    let output = include_str!("../src/cli/output.rs");
    let request_builder = include_str!("../src/cli/request_builder.rs");

    for source in [main, client, config, output] {
        assert!(!source.contains("sandbox_daemon::"));
        assert!(!source.contains("crate::gateway"));
    }
    assert!(!request_builder.contains("sandbox_daemon::"));
    assert!(!request_builder.contains("crate::gateway"));
}

#[tokio::test]
async fn help_writes_stdout_and_exits_successfully() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "--help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Usage: sandbox-cli"));
    assert!(help.contains("--gateway-socket"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_command_writes_top_level_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Usage: sandbox-cli"));
    assert!(help.contains("Commands:"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn manager_help_renders_grouped_catalog_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "manager", "help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Manager Help"));
    assert!(help.contains("Management"));
    assert!(help.contains("create_sandbox"));
    assert!(help.contains("sandbox-cli manager OPERATION"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_manager_operation_renders_grouped_catalog_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "manager"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Manager Help"));
    assert!(help.contains("create_sandbox"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn manager_help_operation_renders_detail_page() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "manager", "help", "create_sandbox"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("create_sandbox"));
    assert!(help.contains("Family\n  Management"));
    assert!(help.contains("Usage\n  sandbox-cli manager create_sandbox"));
    assert!(help.contains("Related Operations"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn manager_required_arg_operation_without_args_renders_detail_page() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "manager", "create_sandbox"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("create_sandbox"));
    assert!(help.contains("Usage\n  sandbox-cli manager create_sandbox"));
    assert!(help.contains("--workspace-root"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn runtime_help_renders_grouped_catalog_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Runtime Help"));
    assert!(help.contains("Command"));
    assert!(help.contains("exec_command"));
    assert!(help.contains("Workspace Session"));
    assert!(help.contains("create_workspace_session"));
    assert!(help.contains("destroy_workspace_session"));
    assert!(!help.contains("--sandbox-id"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn runtime_help_operation_renders_detail_without_sandbox_id() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "help", "exec_command"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("exec_command"));
    assert!(help.contains("Family\n  Command"));
    assert!(help.contains("Usage\n  sandbox-cli runtime exec_command"));
    assert!(help.contains("Related Operations"));
    assert!(!help.contains("--sandbox-id"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn runtime_help_create_workspace_session_renders_detail_without_sandbox_id() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "help", "create_workspace_session"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("create_workspace_session"));
    assert!(help.contains("Family\n  Workspace Session"));
    assert!(help.contains("Usage\n  sandbox-cli runtime create_workspace_session"));
    assert!(help.contains("Examples\n  sandbox-cli runtime create_workspace_session"));
    assert!(!help.contains("--sandbox-id"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn runtime_help_destroy_workspace_session_renders_detail_without_sandbox_id() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        [
            "sandbox-cli",
            "runtime",
            "help",
            "destroy_workspace_session",
        ],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("destroy_workspace_session"));
    assert!(help.contains("Family\n  Workspace Session"));
    assert!(help.contains("Usage\n  sandbox-cli runtime destroy_workspace_session"));
    assert!(help.contains(
        "Examples\n  sandbox-cli runtime destroy_workspace_session --workspace-session-id ws-1"
    ));
    assert!(!help.contains("--sandbox-id"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn observability_help_renders_grouped_catalog_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "observability", "help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Observability Help"));
    assert!(help.contains("Observability"));
    for view in ["snapshot", "trace", "events", "cgroup", "layerstack"] {
        assert!(help.contains(view), "catalog help lists {view}");
    }
    assert!(help.contains("sandbox-cli observability OPERATION"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn observability_help_layerstack_renders_detail_page() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "observability", "help", "layerstack"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Family\n  Observability"));
    assert!(help.contains("Usage\n  sandbox-cli observability layerstack --sandbox-id ID"));
    assert!(help.contains("--sandbox-id"));
    assert!(help.contains("--workspace-id"));
    assert!(help.contains("--window-ms"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn observability_help_snapshot_renders_detail_page() -> TestResult {
    let help = observability_help_page(&["snapshot"]).await?;
    assert!(help.contains("snapshot"));
    assert!(help.contains("Family\n  Observability"));
    assert!(help.contains("Usage\n  sandbox-cli observability snapshot --sandbox-id ID"));
    assert!(help.contains("--sandbox-id"));
    Ok(())
}

#[tokio::test]
async fn observability_help_trace_renders_detail_page() -> TestResult {
    let help = observability_help_page(&["trace"]).await?;
    assert!(help.contains("trace"));
    assert!(help.contains("Family\n  Observability"));
    assert!(help.contains("Usage\n  sandbox-cli observability trace --sandbox-id ID"));
    assert!(help.contains("--trace-id"));
    assert!(help.contains("Default: last"));
    Ok(())
}

#[tokio::test]
async fn observability_help_events_renders_detail_page() -> TestResult {
    let help = observability_help_page(&["events"]).await?;
    assert!(help.contains("events"));
    assert!(help.contains("Family\n  Observability"));
    assert!(help.contains("Usage\n  sandbox-cli observability events --sandbox-id ID"));
    assert!(help.contains("--name"));
    assert!(help.contains("--since-ms"));
    assert!(help.contains("--last-n"));
    Ok(())
}

#[tokio::test]
async fn observability_help_cgroup_renders_detail_page() -> TestResult {
    let help = observability_help_page(&["cgroup"]).await?;
    assert!(help.contains("cgroup"));
    assert!(help.contains("Family\n  Observability"));
    assert!(help.contains("Usage\n  sandbox-cli observability cgroup --sandbox-id ID"));
    assert!(help.contains("--scope"));
    assert!(help.contains("--window-ms"));
    Ok(())
}

#[test]
fn observability_layerstack_maps_to_get_observability_view() -> TestResult {
    let catalog = observability_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Observability,
            operation: "layerstack".to_owned(),
            operation_argv: vec!["--sandbox-id".to_owned(), "eos-abc".to_owned()],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    // One transport, one view: the op is get_observability, the operation name is
    // the view, and --sandbox-id is routing (scope), not a wire param.
    assert_eq!(request.op, "get_observability");
    assert_eq!(
        request.scope,
        CliOperationScope::Sandbox {
            sandbox_id: "eos-abc".to_owned()
        }
    );
    assert_eq!(
        request.args,
        json!({
            "view": "layerstack",
            "window_ms": 60000,
        })
    );
    Ok(())
}

#[test]
fn observability_cgroup_maps_scope_and_window_to_get_observability() -> TestResult {
    let catalog = observability_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Observability,
            operation: "cgroup".to_owned(),
            operation_argv: vec![
                "--sandbox-id".to_owned(),
                "eos-abc".to_owned(),
                "--scope".to_owned(),
                "ws-1".to_owned(),
            ],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    assert_eq!(request.op, "get_observability");
    assert_eq!(
        request.scope,
        CliOperationScope::Sandbox {
            sandbox_id: "eos-abc".to_owned()
        }
    );
    assert_eq!(
        request.args,
        json!({ "view": "cgroup", "scope": "ws-1", "window_ms": 60000 })
    );
    Ok(())
}

#[test]
fn observability_snapshot_maps_to_get_observability_view() -> TestResult {
    let catalog = observability_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Observability,
            operation: "snapshot".to_owned(),
            operation_argv: vec!["--sandbox-id".to_owned(), "eos-abc".to_owned()],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    assert_eq!(request.op, "get_observability");
    assert_eq!(request.args, json!({ "view": "snapshot" }));
    Ok(())
}

#[test]
fn observability_requires_sandbox_id() -> TestResult {
    let catalog = observability_catalog()?;
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Observability,
            operation: "layerstack".to_owned(),
            operation_argv: vec![],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("observability request unexpectedly succeeded")?;

    assert!(error.message().contains("--sandbox-id"));
    Ok(())
}

#[test]
fn observability_trace_defaults_trace_id_to_last() -> TestResult {
    let request = build_observability_request("trace", &["--sandbox-id", "eos-abc"])?;

    assert_eq!(request.op, "get_observability");
    assert_eq!(request.args, json!({ "view": "trace", "trace_id": "last" }));
    Ok(())
}

#[test]
fn observability_trace_maps_explicit_trace_id() -> TestResult {
    let request = build_observability_request(
        "trace",
        &["--sandbox-id", "eos-abc", "--trace-id", "req-7f3"],
    )?;

    assert_eq!(
        request.args,
        json!({ "view": "trace", "trace_id": "req-7f3" })
    );
    Ok(())
}

#[test]
fn observability_events_maps_name_since_ms_and_last_n() -> TestResult {
    let request = build_observability_request(
        "events",
        &[
            "--sandbox-id",
            "eos-abc",
            "--name",
            "lease.acquired",
            "--since-ms",
            "1719500000000",
            "--last-n",
            "20",
        ],
    )?;

    assert_eq!(
        request.args,
        json!({
            "view": "events",
            "name": "lease.acquired",
            "since_ms": 1_719_500_000_000_u64,
            "last_n": 20,
        })
    );
    Ok(())
}

#[test]
fn observability_layerstack_maps_workspace_id() -> TestResult {
    let request = build_observability_request(
        "layerstack",
        &["--sandbox-id", "eos-abc", "--workspace-id", "ws-7"],
    )?;

    assert_eq!(
        request.args,
        json!({ "view": "layerstack", "workspace_id": "ws-7", "window_ms": 60000 })
    );
    Ok(())
}

#[test]
fn observability_rejects_unknown_flag_for_view() -> TestResult {
    let error = build_observability_request("trace", &["--sandbox-id", "eos-abc", "--bogus", "x"])
        .err()
        .ok_or("unknown observability flag unexpectedly accepted")?;

    assert!(error.message().contains("unknown flag"));
    Ok(())
}

#[tokio::test]
async fn runtime_help_unknown_operation_reports_suggestions() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "help", "exec"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 2);
    assert!(stdout.is_empty());
    let error = String::from_utf8(stderr)?;
    assert!(error.contains("unknown runtime operation for help: exec"));
    assert!(error.contains("exec_command"));
    assert!(error.contains("sandbox-cli runtime OPERATION"));
    Ok(())
}

#[tokio::test]
async fn runtime_help_accepts_empty_sandbox_context() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "--sandbox-id", "", "help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Runtime Help"));
    assert!(help.contains("exec_command"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn manual_command_is_rejected() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "manual"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 2);
    assert!(stdout.is_empty());
    let error = String::from_utf8(stderr)?;
    assert!(error.contains("unrecognized subcommand 'manual'"));
    Ok(())
}

#[test]
fn config_precedence_cli_env_default() -> TestResult {
    let default_config = GatewayConfig::discover_with(GatewayConfigOverrides::default(), |_| None)?;
    assert_eq!(
        default_config.gateway_socket_path,
        PathBuf::from(DEFAULT_GATEWAY_SOCKET)
    );

    let env_config =
        GatewayConfig::discover_with(GatewayConfigOverrides::default(), |key| match key {
            SANDBOX_GATEWAY_SOCKET_ENV => Some(OsString::from("/env/gateway.sock")),
            SANDBOX_DEFAULT_ID_ENV => Some(OsString::from("env-sbox")),
            _ => None,
        })?;
    assert_eq!(
        env_config.gateway_socket_path,
        PathBuf::from("/env/gateway.sock")
    );
    assert_eq!(env_config.default_sandbox_id.as_deref(), Some("env-sbox"));

    let cli_config = GatewayConfig::discover_with(
        GatewayConfigOverrides {
            gateway_socket_path: Some(PathBuf::from("/cli/gateway.sock")),
            gateway_auth_token: None,
            default_sandbox_id: Some("cli-sbox".to_owned()),
        },
        |_| None,
    )?;
    assert_eq!(
        cli_config.gateway_socket_path,
        PathBuf::from("/cli/gateway.sock")
    );
    assert_eq!(cli_config.default_sandbox_id.as_deref(), Some("cli-sbox"));
    Ok(())
}

#[tokio::test]
async fn gateway_client_sends_one_request_and_reads_one_response() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).await?;
        let value = serde_json::from_slice::<Value>(&line)?;
        let _ = tx.send(value);
        let mut stream = reader.into_inner();
        stream.write_all(b"{\"ok\":true}\n").await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let client = GatewayClient::new(addr, None);
    let request = Request::new(
        "list_sandboxes",
        "req-1",
        CliOperationScope::System,
        json!({}),
    );
    let response = client.send(&request).await?;
    let sent = rx.await?;
    handle.await??;

    assert_eq!(response, json!({ "ok": true }));
    assert_eq!(sent["op"], "list_sandboxes");
    assert_eq!(sent["request_id"], "req-1");
    assert_eq!(sent["scope"], json!({ "kind": "system" }));
    assert_eq!(sent["args"], json!({}));
    Ok(())
}

#[tokio::test]
async fn gateway_client_injects_auth_token_into_request() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).await?;
        let value = serde_json::from_slice::<Value>(&line)?;
        let _ = tx.send(value);
        let mut stream = reader.into_inner();
        stream.write_all(b"{\"ok\":true}\n").await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let client = GatewayClient::new(addr, Some("secret-token".to_owned()));
    let request = Request::new(
        "list_sandboxes",
        "req-1",
        CliOperationScope::System,
        json!({}),
    );
    let response = client.send(&request).await?;
    let sent = rx.await?;
    handle.await??;

    assert_eq!(response, json!({ "ok": true }));
    assert_eq!(sent["_sandbox_gateway_auth_token"], "secret-token");
    assert_eq!(sent["op"], "list_sandboxes");
    Ok(())
}

#[tokio::test]
async fn gateway_client_streams_events_before_final_response() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).await?;
        let value = serde_json::from_slice::<Value>(&line)?;
        let _ = tx.send(value);
        let mut stream = reader.into_inner();
        stream
            .write_all(
                br#"{"event":"progress","progress":{"op":"create_sandbox","phase":"runtime.create","state":"started"}}"#,
            )
            .await?;
        stream.write_all(b"\n{\"ok\":true}\n").await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let client = GatewayClient::new(addr, None);
    let request = Request::new(
        "create_sandbox",
        "req-1",
        CliOperationScope::System,
        json!({"image": "ubuntu:24.04", "workspace_root": "/testbed"}),
    );
    let mut events = Vec::new();
    let response = client
        .send_with_events(&request, true, |event| events.push(event.clone()))
        .await?;
    let sent = rx.await?;
    handle.await??;

    assert_eq!(response, json!({ "ok": true }));
    assert_eq!(sent["_stream_events"], true);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "progress");
    assert_eq!(events[0]["progress"]["phase"], "runtime.create");
    Ok(())
}

#[tokio::test]
async fn cli_progress_global_streams_events_to_stderr_and_final_to_stdout() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();

    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).await?;
        let value = serde_json::from_slice::<Value>(&line)?;
        let _ = tx.send(value);
        let mut stream = reader.into_inner();
        stream
            .write_all(
                br#"{"event":"progress","progress":{"op":"list_sandboxes","phase":"dispatch","state":"started"}}"#,
            )
            .await?;
        stream.write_all(b"\n{\"sandboxes\":[]}\n").await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        vec![
            "sandbox-cli".to_owned(),
            "--gateway-socket".to_owned(),
            addr,
            "--progress".to_owned(),
            "manager".to_owned(),
            "list_sandboxes".to_owned(),
        ],
        &mut stdout,
        &mut stderr,
    )
    .await;
    let sent = rx.await?;
    handle.await??;

    assert_eq!(exit, 0);
    assert_eq!(sent["_stream_events"], true);
    assert_eq!(String::from_utf8(stdout)?, "{\"sandboxes\":[]}\n");
    let stderr = String::from_utf8(stderr)?;
    assert!(stderr.contains("\"event\":\"progress\""));
    assert!(stderr.contains("\"phase\":\"dispatch\""));
    Ok(())
}

fn build_manager_request(
    operation: &str,
    argv: &[&str],
) -> Result<Request, Box<dyn std::error::Error + Send + Sync>> {
    let catalog = manager_catalog()?;
    Ok(build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Manager,
            operation: operation.to_owned(),
            operation_argv: argv.iter().map(ToString::to_string).collect(),
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )?)
}

fn build_runtime_request(
    sandbox_id: Option<&str>,
    argv: &[&str],
) -> Result<Request, Box<dyn std::error::Error + Send + Sync>> {
    build_runtime_operation_request("exec_command", sandbox_id, argv)
}

fn build_runtime_operation_request(
    operation: &str,
    sandbox_id: Option<&str>,
    argv: &[&str],
) -> Result<Request, Box<dyn std::error::Error + Send + Sync>> {
    let catalog = runtime_catalog()?;
    Ok(build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Runtime,
            operation: operation.to_owned(),
            operation_argv: argv.iter().map(ToString::to_string).collect(),
            sandbox_id: sandbox_id.map(str::to_owned),
        },
        &config(Some("default-sbox")),
        &catalog,
        "req-1",
    )?)
}

fn config(default_sandbox_id: Option<&str>) -> GatewayConfig {
    GatewayConfig {
        gateway_socket_path: PathBuf::from("127.0.0.1:7878"),
        gateway_auth_token: None,
        default_sandbox_id: default_sandbox_id.map(str::to_owned),
    }
}

fn manager_catalog() -> Result<CliOperationCatalogDocument, Box<dyn std::error::Error + Send + Sync>>
{
    Ok(manager_catalog_document()?)
}

fn runtime_catalog() -> Result<CliOperationCatalogDocument, Box<dyn std::error::Error + Send + Sync>>
{
    Ok(runtime_catalog_document()?)
}

fn observability_catalog(
) -> Result<CliOperationCatalogDocument, Box<dyn std::error::Error + Send + Sync>> {
    Ok(observability_catalog_document()?)
}

fn build_observability_request(
    operation: &str,
    argv: &[&str],
) -> Result<Request, RequestBuildError> {
    let catalog = observability_catalog_document()?;
    build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: CliOperationExecutionSpace::Observability,
            operation: operation.to_owned(),
            operation_argv: argv.iter().map(ToString::to_string).collect(),
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )
}

async fn observability_help_page(
    operation: &[&str],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut argv = vec!["sandbox-cli", "observability", "help"];
    argv.extend_from_slice(operation);
    let exit =
        sandbox_gateway::cli::output::run_cli_with_writers(argv, &mut stdout, &mut stderr).await;

    assert_eq!(exit, 0);
    assert!(stderr.is_empty());
    Ok(String::from_utf8(stdout)?)
}
