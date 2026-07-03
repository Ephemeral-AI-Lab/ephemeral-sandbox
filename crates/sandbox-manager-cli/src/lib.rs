//! Operator CLI: fleet lifecycle (manager operations) plus observability views.
//!
//! A thin protocol client over [`sandbox_cli_core`]. It links the manager and
//! observability spec catalogs only — never a manager/runtime engine — and
//! stamps system scope for manager operations. Observability views retain the
//! server-side dual routing (aggregate `snapshot` without `--sandbox-id`, daemon
//! `get_observability` with it) via the shared core request builder.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;

use sandbox_cli_core::client::GatewayClient;
use sandbox_cli_core::output::{
    discover_config, operation_requires_args, render_help_command, render_request_error,
    run_request_from_catalog, take_progress_flag, EXIT_SUCCESS, EXIT_USAGE,
};
use sandbox_cli_core::request_builder::{catalog_document, BuildRequestInput, RequestBuildError};
use sandbox_cli_core::GatewayConfigOverrides;
use sandbox_protocol::{CliOperationCatalogDocument, CliOperationExecutionSpace};

const PROGRAM: &str = "sandbox-manager-cli";
const OBSERVABILITY_SUBCOMMAND: &str = "observability";
const OBSERVABILITY_PROGRAM: &str = "sandbox-manager-cli observability";
const HELP_OP: &str = "help";
const CREATE_SANDBOX_OP: &str = "create_sandbox";

#[derive(Debug, Parser)]
#[command(name = "sandbox-manager-cli", disable_help_subcommand = true)]
struct Cli {
    #[arg(long = "gateway-socket", value_name = "HOST:PORT", global = true)]
    gateway_socket_path: Option<PathBuf>,

    #[arg(long = "gateway-auth-token", value_name = "TOKEN", global = true)]
    gateway_auth_token: Option<String>,

    #[arg(long = "progress", global = true)]
    progress: bool,

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
            let _ = write!(stderr, "{error}");
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

    if operation == OBSERVABILITY_SUBCOMMAND {
        return run_observability(
            cli.operation_argv,
            overrides,
            global_progress,
            stdout,
            stderr,
        )
        .await;
    }

    run_manager(
        operation,
        cli.operation_argv,
        overrides,
        global_progress,
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
    if operation_argv.is_empty() && operation_requires_args(&catalog, &operation) {
        return render_help_command(&catalog, &[operation], PROGRAM, stdout, stderr);
    }
    let Some(client) = client_from(overrides, stderr) else {
        return EXIT_USAGE;
    };
    let request_input = BuildRequestInput {
        execution_space: CliOperationExecutionSpace::Manager,
        operation,
        operation_argv,
        sandbox_id: None,
    };
    run_request_from_catalog(&client, request_input, &catalog, progress, stdout, stderr).await
}

async fn run_observability<WOut, WErr>(
    observability_argv: Vec<String>,
    overrides: GatewayConfigOverrides,
    global_progress: bool,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let catalog = match observability_catalog(stderr) {
        Ok(catalog) => catalog,
        Err(exit) => return exit,
    };
    let Some((operation, rest)) = observability_argv.split_first() else {
        return render_help_command(&catalog, &[], OBSERVABILITY_PROGRAM, stdout, stderr);
    };
    let operation = operation.clone();
    let rest = rest.to_vec();
    if operation == HELP_OP {
        return render_help_command(&catalog, &rest, OBSERVABILITY_PROGRAM, stdout, stderr);
    }
    if rest.is_empty() && operation_requires_args(&catalog, &operation) {
        return render_help_command(
            &catalog,
            &[operation],
            OBSERVABILITY_PROGRAM,
            stdout,
            stderr,
        );
    }
    let Some(client) = client_from(overrides, stderr) else {
        return EXIT_USAGE;
    };
    let request_input = BuildRequestInput {
        execution_space: CliOperationExecutionSpace::Observability,
        operation,
        operation_argv: rest,
        sandbox_id: None,
    };
    run_request_from_catalog(
        &client,
        request_input,
        &catalog,
        global_progress,
        stdout,
        stderr,
    )
    .await
}

fn manager_catalog<WErr>(stderr: &mut WErr) -> Result<CliOperationCatalogDocument, u8>
where
    WErr: Write,
{
    catalog_or_usage_error(
        catalog_document(sandbox_manager_operations::manager_catalog()),
        stderr,
    )
}

fn observability_catalog<WErr>(stderr: &mut WErr) -> Result<CliOperationCatalogDocument, u8>
where
    WErr: Write,
{
    catalog_or_usage_error(
        catalog_document(sandbox_observability_operations::observability_catalog()),
        stderr,
    )
}

fn catalog_or_usage_error<WErr>(
    catalog: Result<CliOperationCatalogDocument, RequestBuildError>,
    stderr: &mut WErr,
) -> Result<CliOperationCatalogDocument, u8>
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
    Some(GatewayClient::new(
        config.gateway_socket_path.to_string_lossy().into_owned(),
        config.gateway_auth_token.clone(),
    ))
}
