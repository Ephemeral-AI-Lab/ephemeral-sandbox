//! Agent CLI: drive exactly one sandbox (commands and files).
//!
//! A thin gateway client that links only the runtime semantic catalog — never
//! a manager/runtime engine — and stamps sandbox scope. A
//! `--sandbox-id` is required on every operation; there is no env or config
//! fallback.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;
use serde::Deserialize;
use serde_json::json;

use crate::input::{resolve_runtime_sandbox_id, BuildRequestInput};
use crate::output::{
    discover_config, render_error, render_help_command, render_request_error,
    run_request_from_catalog_with_id, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE,
};
use crate::projection::document::catalog_document;
use sandbox_operation_client::{GatewayClient, GatewayConfigOverrides, RequestBuildError};
use sandbox_operation_contract::OperationDomain;

const PROGRAM: &str = "sandbox-runtime-cli --sandbox-id ID [--request-id VALUE]";
const HELP_OP: &str = "help";
const REQUEST_ID_ERROR: &str =
    "--request-id must be 1-128 ASCII letters, digits, period, underscore, colon, or dash";
const BATCH_MODE_ERROR: &str = "--batch-jsonl cannot be combined with --request-id or an operation";
const BATCH_REQUEST_MAX_BYTES: usize = 1024 * 1024;
const BATCH_READY_KIND: &str = "sandbox_runtime_cli_batch_ready_v1";
const BATCH_RESPONSE_KIND: &str = "sandbox_runtime_cli_batch_response_v1";

#[derive(Debug, Parser)]
#[command(name = "sandbox-runtime-cli", disable_help_subcommand = true)]
struct Cli {
    #[arg(long = "gateway-socket", value_name = "HOST:PORT", global = true)]
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

    /// Read operation requests from stdin and emit one response envelope per
    /// line while retaining the authenticated, sandbox-pinned CLI process.
    #[arg(long = "batch-jsonl", global = true)]
    batch_jsonl: bool,

    operation: Option<String>,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    operation_argv: Vec<String>,
}

pub async fn run_cli<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    ExitCode::from(run_cli_with_streams(args, &mut stdin, &mut stdout, &mut stderr).await)
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
    run_cli_with_streams(args, io::empty(), stdout, stderr).await
}

pub async fn run_cli_with_streams<I, T, RIn, WOut, WErr>(
    args: I,
    mut stdin: RIn,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    RIn: BufRead,
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
    if cli.batch_jsonl && (cli.request_id.is_some() || cli.operation.is_some()) {
        let error = RequestBuildError::invalid(BATCH_MODE_ERROR);
        let _ = render_request_error(&error, stderr);
        return EXIT_USAGE;
    }
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

    if cli.batch_jsonl {
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
        return run_batch_jsonl(&client, &sandbox_id, &catalog, &mut stdin, stdout, stderr).await;
    }

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest {
    schema_version: u32,
    request_id: Option<String>,
    operation: String,
    operation_argv: Vec<String>,
}

async fn run_batch_jsonl<RIn, WOut, WErr>(
    client: &GatewayClient,
    sandbox_id: &str,
    catalog: &crate::projection::document::CatalogDocument,
    stdin: &mut RIn,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    RIn: BufRead,
    WOut: Write,
    WErr: Write,
{
    if write_json_line(stdout, &json!({"kind": BATCH_READY_KIND})).is_err()
        || stdout.flush().is_err()
    {
        return EXIT_FAILURE;
    }

    loop {
        let mut line = String::new();
        let bytes = match stdin.read_line(&mut line) {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = render_error("batch_io_error", error.to_string(), stderr);
                return EXIT_FAILURE;
            }
        };
        if bytes == 0 {
            return EXIT_SUCCESS;
        }

        let mut operation_stdout = Vec::new();
        let mut operation_stderr = Vec::new();
        let code = match parse_batch_request(&line) {
            Ok(request) => {
                let request_input = BuildRequestInput {
                    execution_space: OperationDomain::Runtime,
                    operation: request.operation,
                    operation_argv: request.operation_argv,
                    sandbox_id: Some(sandbox_id.to_owned()),
                };
                run_request_from_catalog_with_id(
                    client,
                    request_input,
                    request.request_id,
                    catalog,
                    false,
                    &mut operation_stdout,
                    &mut operation_stderr,
                )
                .await
            }
            Err(error) => {
                let _ = render_request_error(&error, &mut operation_stderr);
                EXIT_USAGE
            }
        };
        let response = json!({
            "kind": BATCH_RESPONSE_KIND,
            "exit_code": code,
            "stdout": String::from_utf8_lossy(&operation_stdout),
            "stderr": String::from_utf8_lossy(&operation_stderr),
        });
        if write_json_line(stdout, &response).is_err() || stdout.flush().is_err() {
            return EXIT_FAILURE;
        }
    }
}

fn parse_batch_request(line: &str) -> Result<BatchRequest, RequestBuildError> {
    if line.len() > BATCH_REQUEST_MAX_BYTES {
        return Err(RequestBuildError::invalid(format!(
            "batch request exceeded {BATCH_REQUEST_MAX_BYTES} bytes"
        )));
    }
    let request: BatchRequest = serde_json::from_str(line)
        .map_err(|error| RequestBuildError::invalid(format!("invalid batch request: {error}")))?;
    if request.schema_version != 1 {
        return Err(RequestBuildError::invalid(
            "batch request schema_version must be 1",
        ));
    }
    validate_request_id(request.request_id.clone())?;
    Ok(request)
}

fn write_json_line<W>(writer: &mut W, value: &serde_json::Value) -> io::Result<()>
where
    W: Write,
{
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")
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
    Some(GatewayClient::new(
        config.gateway_socket_path.to_string_lossy().into_owned(),
        config.gateway_auth_token.clone(),
    ))
}
