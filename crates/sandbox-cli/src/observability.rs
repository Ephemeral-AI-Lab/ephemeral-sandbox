//! Read-only CLI for aggregate and sandbox-scoped observability views.
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
    run_request_from_catalog_with_id, EXIT_SUCCESS, EXIT_USAGE,
};
use crate::projection::document::catalog_document;
use sandbox_operation_client::{GatewayClient, GatewayConfigOverrides, RequestBuildError};
use sandbox_operation_contract::OperationDomain;

const PROGRAM: &str = "sandbox-observability-cli [--request-id VALUE]";
const HELP_OP: &str = "help";

#[derive(Debug, Parser)]
#[command(name = "sandbox-observability-cli", disable_help_subcommand = true)]
struct Cli {
    #[arg(long = "gateway-socket", value_name = "HOST:PORT", global = true)]
    gateway_socket_path: Option<PathBuf>,

    #[arg(long = "gateway-auth-token", value_name = "TOKEN", global = true)]
    gateway_auth_token: Option<String>,

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

    let catalog = match catalog_document(
        sandbox_operation_catalog::observability::observability_catalog(),
        crate::projection::observability::catalog_projection(),
    ) {
        Ok(catalog) => catalog,
        Err(error) => {
            let error = RequestBuildError::invalid(error.message());
            let _ = render_request_error(&error, stderr);
            return EXIT_USAGE;
        }
    };

    let Some(operation) = cli.operation else {
        return render_help_command(&catalog, &[], PROGRAM, stdout, stderr);
    };
    if operation == HELP_OP {
        return render_help_command(&catalog, &cli.operation_argv, PROGRAM, stdout, stderr);
    }

    let overrides = GatewayConfigOverrides {
        gateway_socket_path: cli.gateway_socket_path,
        gateway_auth_token: cli.gateway_auth_token,
    };
    let Some(client) = client_from(overrides, stderr) else {
        return EXIT_USAGE;
    };
    let request_input = BuildRequestInput {
        execution_space: OperationDomain::Observability,
        operation,
        operation_argv: cli.operation_argv,
        sandbox_id: None,
    };
    run_request_from_catalog_with_id(
        &client,
        request_input,
        request_id,
        &catalog,
        false,
        stdout,
        stderr,
    )
    .await
}

fn client_from<WErr>(overrides: GatewayConfigOverrides, stderr: &mut WErr) -> Option<GatewayClient>
where
    WErr: Write,
{
    let config = discover_config(overrides, stderr).ok()?;
    Some(GatewayClient::new(
        config.gateway_socket_path.to_string_lossy().into_owned(),
        config.gateway_auth_token.clone(),
    ))
}
