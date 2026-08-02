//! Agent CLI: drive exactly one sandbox (commands and files).
//!
//! A thin gateway client that links only the runtime semantic catalog — never
//! a manager/runtime engine — and stamps sandbox scope. A
//! `--sandbox-id` is required on every operation; there is no env or config
//! fallback.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;

use crate::input::{resolve_runtime_sandbox_id, BuildRequestInput};
use crate::output::{
    discover_config, render_error, render_help_command, render_request_error,
    run_request_from_catalog_with_id, EXIT_SUCCESS, EXIT_USAGE,
};
use crate::projection::document::catalog_document;
use sandbox_operation_client::{GatewayClient, GatewayConfigOverrides, RequestBuildError};
use sandbox_operation_contract::OperationDomain;

const PROGRAM: &str = "sandbox-runtime-cli --sandbox-id ID [--request-id VALUE]";
const HELP_OP: &str = "help";
const REQUEST_ID_ERROR: &str =
    "--request-id must be 1-128 ASCII letters, digits, period, underscore, colon, or dash";

#[derive(Debug, Parser)]
#[command(name = "sandbox-runtime-cli", disable_help_subcommand = true)]
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

    #[arg(long = "sandbox-id", value_name = "SANDBOX_ID", global = true)]
    sandbox_id: Option<String>,

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
        sandbox_operation_catalog::runtime::runtime_catalog(),
        crate::projection::runtime::catalog_projection(),
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
    let sandbox_id = match resolve_runtime_sandbox_id(cli.sandbox_id) {
        Ok(sandbox_id) => sandbox_id,
        Err(error) => {
            let _ = render_request_error(&error, stderr);
            return EXIT_USAGE;
        }
    };

    let overrides = GatewayConfigOverrides {
        gateway_socket_path: cli.gateway_socket_path,
        gateway_auth_token: cli.gateway_auth_token,
    };
    let Some(client) = client_from(overrides, stderr) else {
        return EXIT_USAGE;
    };
    let request_input = BuildRequestInput {
        execution_space: OperationDomain::Runtime,
        operation,
        operation_argv: cli.operation_argv,
        sandbox_id: Some(sandbox_id),
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

fn validate_request_id(request_id: Option<String>) -> Result<Option<String>, RequestBuildError> {
    let Some(request_id) = request_id else {
        return Ok(None);
    };
    if request_id.len() > 128
        || request_id.is_empty()
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(RequestBuildError::invalid(REQUEST_ID_ERROR));
    }
    Ok(Some(request_id))
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
