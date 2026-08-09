//! Operator CLI for fleet lifecycle management.
//!
//! A thin gateway client that links only the management catalog, never a
//! manager/runtime engine, and stamps system scope.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;

use crate::input::{validate_request_id, BuildRequestInput};
use crate::output::{
    discover_config, render_error, render_help_command, render_request_error,
    run_request_from_catalog_with_id, take_progress_flag, EXIT_SUCCESS, EXIT_USAGE,
};
use crate::projection::document::{catalog_document, CatalogDocument};
use sandbox_operation_client::{GatewayClient, GatewayConfigOverrides, RequestBuildError};
use sandbox_operation_contract::OperationDomain;

const PROGRAM: &str = "sandbox-manager-cli [--request-id VALUE]";
const HELP_OP: &str = "help";
const CREATE_SANDBOX_OP: &str = "create_sandbox";

#[derive(Debug, Parser)]
#[command(name = "sandbox-manager-cli", disable_help_subcommand = true)]
struct Cli {
    #[arg(
        long = "gateway-endpoint",
        visible_alias = "gateway-socket",
        value_name = "URI",
        global = true
    )]
    gateway_socket_path: Option<PathBuf>,

    #[arg(long = "gateway-auth-token", value_name = "TOKEN", global = true)]
    gateway_auth_token: Option<String>,

    #[arg(long = "progress", global = true)]
    progress: bool,

    #[arg(
        long = "request-id",
        value_name = "VALUE",
        global = true,
        allow_hyphen_values = true
    )]
    request_id: Option<String>,

    operation: Option<String>,

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
            let _ = render_error("invalid_request", error.to_string(), stderr);
            return EXIT_USAGE;
        }
    };
    let request_id = match validate_request_id(cli.request_id) {
        Ok(request_id) => request_id,
        Err(error) => {
            let _ = render_request_error(&error, stderr);
            return EXIT_USAGE;
        }
    };

    let overrides = GatewayConfigOverrides {
        gateway_socket_path: cli.gateway_socket_path,
        gateway_auth_token: cli.gateway_auth_token,
    };
    let global_progress = cli.progress;

    let Some(operation) = cli.operation else {
        return match manager_catalog(stderr) {
            Ok(catalog) => render_help_command(&catalog, &[], PROGRAM, stdout, stderr),
            Err(exit) => exit,
        };
    };

    run_manager(
        operation,
        cli.operation_argv,
        overrides,
        global_progress,
        request_id,
        stdout,
        stderr,
    )
    .await
}

async fn run_manager<WOut, WErr>(
    operation: String,
    mut operation_argv: Vec<String>,
    overrides: GatewayConfigOverrides,
    global_progress: bool,
    request_id: Option<String>,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let catalog = match manager_catalog(stderr) {
        Ok(catalog) => catalog,
        Err(exit) => return exit,
    };
    if operation == HELP_OP {
        return render_help_command(&catalog, &operation_argv, PROGRAM, stdout, stderr);
    }
    let progress = global_progress
        || (operation == CREATE_SANDBOX_OP && take_progress_flag(&mut operation_argv));
    let Some(client) = client_from(overrides, stderr) else {
        return EXIT_USAGE;
    };
    let request_input = BuildRequestInput {
        execution_space: OperationDomain::Manager,
        operation,
        operation_argv,
        sandbox_id: None,
    };
    run_request_from_catalog_with_id(
        &client,
        request_input,
        request_id,
        &catalog,
        progress,
        stdout,
        stderr,
    )
    .await
}

fn manager_catalog<WErr>(stderr: &mut WErr) -> Result<CatalogDocument, u8>
where
    WErr: Write,
{
    catalog_or_usage_error(
        catalog_document(
            sandbox_operation_catalog::manager::manager_catalog(),
            crate::projection::manager::catalog_projection(),
        )
        .map_err(|error| RequestBuildError::invalid(error.message())),
        stderr,
    )
}

fn catalog_or_usage_error<WErr>(
    catalog: Result<CatalogDocument, RequestBuildError>,
    stderr: &mut WErr,
) -> Result<CatalogDocument, u8>
where
    WErr: Write,
{
    catalog.map_err(|error| {
        let _ = render_request_error(&error, stderr);
        EXIT_USAGE
    })
}

fn client_from<WErr>(overrides: GatewayConfigOverrides, stderr: &mut WErr) -> Option<GatewayClient>
where
    WErr: Write,
{
    let config = discover_config(overrides, stderr).ok()?;
    Some(GatewayClient::from_endpoint(
        config.gateway_endpoint,
        config.gateway_auth_token,
    ))
}
