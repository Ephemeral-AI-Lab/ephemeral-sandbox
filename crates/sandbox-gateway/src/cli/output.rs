use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use crate::cli::client::GatewayClient;
use crate::cli::config::{GatewayConfig, GatewayConfigOverrides};
use crate::cli::request_builder::{
    build_request_from_catalog, manager_catalog_document, resolve_runtime_sandbox_id,
    runtime_catalog_document, BuildRequestInput, RequestBuildError,
};
use crate::cli::timing;
use sandbox_protocol::{
    render_catalog_help, render_operation_help, CliOperationCatalogDocument,
    CliOperationExecutionSpace,
};

const EXIT_SUCCESS: u8 = 0;
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(name = "sandbox-cli", disable_help_subcommand = true)]
struct Cli {
    #[arg(long = "gateway-socket", value_name = "HOST:PORT", global = true)]
    gateway_socket_path: Option<PathBuf>,

    #[arg(long = "gateway-auth-token", value_name = "TOKEN", global = true)]
    gateway_auth_token: Option<String>,

    #[arg(long = "default-sandbox-id", value_name = "SANDBOX_ID", global = true)]
    default_sandbox_id: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Manager(OperationCommand),
    Runtime(RuntimeCommand),
}

#[derive(Debug, Args)]
struct OperationCommand {
    operation: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    operation_argv: Vec<String>,
}

#[derive(Debug, Args)]
struct RuntimeCommand {
    #[arg(long = "sandbox-id", value_name = "SANDBOX_ID")]
    sandbox_id: Option<String>,

    operation: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    operation_argv: Vec<String>,
}

pub async fn run_cli<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    ExitCode::from(run_cli_with_writers(args, &mut stdout, &mut stderr).await)
}

pub async fn run_cli_with_writers<I, T, WOut, WErr>(
    args: I,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    WOut: Write,
    WErr: Write,
{
    timing::checkpoint("cli.start");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = write!(stdout, "{error}");
                return EXIT_SUCCESS;
            }
            let _ = write!(stderr, "{error}");
            return EXIT_USAGE;
        }
    };
    timing::checkpoint("cli.parsed");

    let config_overrides = GatewayConfigOverrides {
        gateway_socket_path: cli.gateway_socket_path,
        gateway_auth_token: cli.gateway_auth_token,
        default_sandbox_id: cli.default_sandbox_id,
    };

    match cli.command {
        Command::Manager(command) => {
            let catalog = match manager_catalog_document() {
                Ok(catalog) => catalog,
                Err(error) => {
                    let _ = render_request_error(&error, stderr);
                    return EXIT_USAGE;
                }
            };
            timing::checkpoint("cli.manager_catalog_loaded");
            if command.operation == "help" {
                return render_help_command(&catalog, &command.operation_argv, stdout, stderr);
            }
            let config = match discover_config(config_overrides, stderr) {
                Ok(config) => config,
                Err(exit) => return exit,
            };
            timing::checkpoint("cli.config_discovered");
            let client = GatewayClient::new(
                config.gateway_socket_path.to_string_lossy().into_owned(),
                config.gateway_auth_token.clone(),
            );
            let request_input = BuildRequestInput {
                execution_space: CliOperationExecutionSpace::Manager,
                operation: command.operation,
                operation_argv: command.operation_argv,
                sandbox_id: None,
            };
            run_request_from_catalog(&client, request_input, &config, &catalog, stdout, stderr)
                .await
        }
        Command::Runtime(command) => {
            if command.operation == "help" {
                let catalog = match runtime_catalog_document() {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        let _ = render_request_error(&error, stderr);
                        return EXIT_USAGE;
                    }
                };
                timing::checkpoint("cli.runtime_help_catalog_loaded");
                return render_help_command(&catalog, &command.operation_argv, stdout, stderr);
            }
            let config = match discover_config(config_overrides, stderr) {
                Ok(config) => config,
                Err(exit) => return exit,
            };
            timing::checkpoint("cli.config_discovered");
            let sandbox_id = match resolve_runtime_sandbox_id(command.sandbox_id, &config) {
                Ok(sandbox_id) => sandbox_id,
                Err(error) => {
                    let _ = render_request_error(&error, stderr);
                    return EXIT_USAGE;
                }
            };
            timing::checkpoint("cli.sandbox_id_resolved");
            let catalog = match runtime_catalog_document() {
                Ok(catalog) => catalog,
                Err(error) => {
                    let _ = render_request_error(&error, stderr);
                    return EXIT_USAGE;
                }
            };
            timing::checkpoint("cli.runtime_catalog_loaded");
            let request_input = BuildRequestInput {
                execution_space: CliOperationExecutionSpace::Runtime,
                operation: command.operation,
                operation_argv: command.operation_argv,
                sandbox_id: Some(sandbox_id),
            };
            let client = GatewayClient::new(
                config.gateway_socket_path.to_string_lossy().into_owned(),
                config.gateway_auth_token.clone(),
            );
            return run_request_from_catalog(
                &client,
                request_input,
                &config,
                &catalog,
                stdout,
                stderr,
            )
            .await;
        }
    }
}

fn discover_config<WErr>(
    overrides: GatewayConfigOverrides,
    stderr: &mut WErr,
) -> Result<GatewayConfig, u8>
where
    WErr: Write,
{
    match GatewayConfig::discover(overrides) {
        Ok(config) => Ok(config),
        Err(error) => {
            let _ = render_error("config_error", error.to_string(), stderr);
            Err(EXIT_USAGE)
        }
    }
}

fn render_help_command<WOut, WErr>(
    catalog: &CliOperationCatalogDocument,
    operation_argv: &[String],
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let rendered = match operation_argv {
        [] => Ok(render_catalog_help(catalog)),
        [operation] => render_operation_help(catalog, operation),
        _ => {
            let _ = writeln!(stderr, "help accepts at most one operation");
            return EXIT_USAGE;
        }
    };

    match rendered {
        Ok(help) => {
            let _ = stdout.write_all(help.as_bytes());
            EXIT_SUCCESS
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_USAGE
        }
    }
}

async fn run_request_from_catalog<WOut, WErr>(
    client: &GatewayClient,
    request_input: BuildRequestInput,
    config: &GatewayConfig,
    catalog: &CliOperationCatalogDocument,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let build_started = std::time::Instant::now();
    let request = match build_request_from_catalog(request_input, config, catalog) {
        Ok(request) => request,
        Err(error) => {
            let _ = render_request_error(&error, stderr);
            return EXIT_USAGE;
        }
    };
    timing::duration("cli.build_request", build_started);

    let send_started = std::time::Instant::now();
    let response = match client.send(&request).await {
        Ok(response) => response,
        Err(error) => {
            let _ = render_error(error.kind(), error.to_string(), stderr);
            return EXIT_FAILURE;
        }
    };
    timing::duration("cli.gateway_roundtrip", send_started);

    let render_started = std::time::Instant::now();
    let exit = render_response(&response, stdout, stderr).unwrap_or(EXIT_FAILURE);
    timing::duration("cli.render_response", render_started);
    timing::checkpoint("cli.done");
    exit
}

pub fn render_response<WOut, WErr>(
    response: &Value,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> io::Result<u8>
where
    WOut: Write,
    WErr: Write,
{
    if response.get("error").is_some() {
        write_json_line(stderr, response)?;
        Ok(EXIT_FAILURE)
    } else {
        write_json_line(stdout, response)?;
        Ok(EXIT_SUCCESS)
    }
}

fn render_error<WErr>(
    kind: &'static str,
    message: impl Into<String>,
    stderr: &mut WErr,
) -> io::Result<()>
where
    WErr: Write,
{
    let response = sandbox_protocol::error_response_with_details(kind, message, json!({}));
    write_json_line(stderr, &response)
}

fn render_request_error<WErr>(error: &RequestBuildError, stderr: &mut WErr) -> io::Result<()>
where
    WErr: Write,
{
    render_error("invalid_request", error.message(), stderr)
}

fn write_json_line<W>(writer: &mut W, value: &Value) -> io::Result<()>
where
    W: Write,
{
    writer.write_all(&json_line(value))
}

fn json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    line
}
