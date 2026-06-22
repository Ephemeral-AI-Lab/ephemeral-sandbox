use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_gateway::cli::client::GatewayClient;
use sandbox_gateway::cli::config::{
    GatewayConfig, GatewayConfigOverrides, DEFAULT_GATEWAY_SOCKET, SANDBOX_DEFAULT_ID_ENV,
    SANDBOX_GATEWAY_SOCKET_ENV,
};
use sandbox_gateway::cli::output::render_response;
use sandbox_gateway::cli::request_builder::{
    build_request_from_catalog_with_id, manager_catalog_document, runtime_catalog_document,
    BuildRequestInput,
};
use sandbox_protocol::{
    OperationCatalogDocument, OperationExecutionSpace, OperationScope, Request,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn manager_operation_uses_system_scope() -> TestResult {
    let request = build_manager_request("list_sandboxes", &[])?;

    assert_eq!(request.scope, OperationScope::System);
    assert_eq!(request.args, json!({}));
    Ok(())
}

#[test]
fn runtime_operation_requires_sandbox_without_default() -> TestResult {
    let catalog = runtime_catalog()?;
    let config = config(None);
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Runtime,
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
        OperationScope::Sandbox {
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
        OperationScope::Sandbox {
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
            execution_space: OperationExecutionSpace::Manager,
            operation: "exec_command".to_owned(),
            operation_argv: vec![],
            sandbox_id: None,
        },
        &config(None),
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("manager request unexpectedly accepted runtime catalog")?;

    assert_eq!(
        error.message(),
        "loaded catalog is for runtime, not manager"
    );
    Ok(())
}

#[test]
fn runtime_request_construction_rejects_manager_catalog() -> TestResult {
    let catalog = manager_catalog()?;
    let error = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Runtime,
            operation: "create_sandbox".to_owned(),
            operation_argv: vec![],
            sandbox_id: Some("sbox-1".to_owned()),
        },
        &config(None),
        &catalog,
        "req-1",
    )
    .err()
    .ok_or("runtime request unexpectedly accepted manager catalog")?;

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

    assert_eq!(request.scope, OperationScope::System);
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
fn read_command_lines_maps_command_session_id_start_offset_and_limit_flags() -> TestResult {
    let catalog = runtime_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Runtime,
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
            execution_space: OperationExecutionSpace::Runtime,
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
fn gateway_cli_maps_cgroup_monitor_args_from_runtime_catalog() -> TestResult {
    let catalog = runtime_catalog()?;
    let request = build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Runtime,
            operation: "read_cgroup_monitor_samples".to_owned(),
            operation_argv: vec![
                "--workspace-session-id".to_owned(),
                "ws-1".to_owned(),
                "--command-session-id".to_owned(),
                "cmd-1".to_owned(),
                "--limit".to_owned(),
                "50".to_owned(),
            ],
            sandbox_id: Some("sbox-1".to_owned()),
        },
        &config(None),
        &catalog,
        "req-1",
    )?;

    assert_eq!(request.op, "read_cgroup_monitor_samples");
    assert_eq!(
        request.args,
        json!({
            "workspace_session_id": "ws-1",
            "command_session_id": "cmd-1",
            "limit": 50,
        })
    );
    assert!(request.args.get("sandbox_id").is_none());
    Ok(())
}

#[test]
fn gateway_cli_cgroup_monitor_catalog_usage_omits_sandbox_id() -> TestResult {
    let catalog = runtime_catalog()?;
    for operation in ["inspect_cgroup_monitor", "read_cgroup_monitor_samples"] {
        let spec = catalog
            .operations
            .iter()
            .find(|spec| spec.name == operation)
            .ok_or("cgroup monitor operation missing")?;
        let cli = spec.cli.as_ref().ok_or("cgroup monitor cli missing")?;
        assert!(cli.usage.starts_with("sandbox-cli runtime "));
        assert!(!cli.usage.contains("--sandbox-id"));
        assert!(cli
            .examples
            .iter()
            .all(|example| example.starts_with("sandbox-cli runtime ")
                && !example.contains("--sandbox-id")
                && !example.contains("daemon")
                && !example.contains("manual")));
    }
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
    assert!(help.contains("sandbox-cli manager help OPERATION"));
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
async fn runtime_help_renders_grouped_catalog_help() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        [
            "sandbox-cli",
            "--default-sandbox-id",
            "sbox-1",
            "runtime",
            "help",
        ],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 0);
    let help = String::from_utf8(stdout)?;
    assert!(help.contains("Sandbox Runtime Help"));
    assert!(help.contains("Command"));
    assert!(help.contains("Cgroup Monitor"));
    assert!(help.contains("exec_command"));
    assert!(help.contains("inspect_cgroup_monitor"));
    assert!(!help.contains("--sandbox-id"));
    assert!(stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn runtime_help_operation_renders_detail_without_sandbox_id() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        [
            "sandbox-cli",
            "--default-sandbox-id",
            "sbox-1",
            "runtime",
            "help",
            "exec_command",
        ],
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
async fn runtime_help_unknown_operation_reports_suggestions() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        [
            "sandbox-cli",
            "--default-sandbox-id",
            "sbox-1",
            "runtime",
            "help",
            "exec",
        ],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 2);
    assert!(stdout.is_empty());
    let error = String::from_utf8(stderr)?;
    assert!(error.contains("unknown runtime operation for help: exec"));
    assert!(error.contains("exec_command"));
    assert!(error.contains("sandbox-cli runtime help"));
    Ok(())
}

#[tokio::test]
async fn runtime_help_requires_default_sandbox() -> TestResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = sandbox_gateway::cli::output::run_cli_with_writers(
        ["sandbox-cli", "runtime", "--sandbox-id", "", "help"],
        &mut stdout,
        &mut stderr,
    )
    .await;

    assert_eq!(exit, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr)?,
        "runtime help requires a default sandbox\n"
    );
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
    let root = unique_temp_dir("sandbox-cli-client-test")?;
    std::fs::create_dir_all(&root)?;
    let socket_path = root.join("gateway.sock");
    let listener = UnixListener::bind(&socket_path)?;
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

    let client = GatewayClient::new(&socket_path);
    let request = Request::new("list_sandboxes", "req-1", OperationScope::System, json!({}));
    let response = client.send(&request).await?;
    let sent = rx.await?;
    handle.await??;

    assert_eq!(response, json!({ "ok": true }));
    assert_eq!(sent["op"], "list_sandboxes");
    assert_eq!(sent["request_id"], "req-1");
    assert_eq!(sent["scope"], json!({ "kind": "system" }));
    assert_eq!(sent["args"], json!({}));
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

fn build_manager_request(
    operation: &str,
    argv: &[&str],
) -> Result<Request, Box<dyn std::error::Error + Send + Sync>> {
    let catalog = manager_catalog()?;
    Ok(build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Manager,
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
    let catalog = runtime_catalog()?;
    Ok(build_request_from_catalog_with_id(
        BuildRequestInput {
            execution_space: OperationExecutionSpace::Runtime,
            operation: "exec_command".to_owned(),
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
        gateway_socket_path: PathBuf::from("/tmp/gateway.sock"),
        default_sandbox_id: default_sandbox_id.map(str::to_owned),
    }
}

fn manager_catalog() -> Result<OperationCatalogDocument, Box<dyn std::error::Error + Send + Sync>> {
    Ok(manager_catalog_document()?)
}

fn runtime_catalog() -> Result<OperationCatalogDocument, Box<dyn std::error::Error + Send + Sync>> {
    Ok(runtime_catalog_document()?)
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let short_prefix = prefix.chars().take(3).collect::<String>();
    Ok(std::env::temp_dir().join(format!("{short_prefix}-{}-{nanos:x}", std::process::id())))
}
