use std::io::{self, Write};
use std::time::Instant;

use serde_json::{json, Value};

use sandbox_config::configs::cli::{GatewayConfig, GatewayConfigOverrides};
use sandbox_protocol::{render_catalog_help, render_operation_help, CliOperationCatalogDocument};

use crate::client::GatewayClient;
use crate::request_builder::{build_request_from_catalog, BuildRequestInput, RequestBuildError};

pub const EXIT_SUCCESS: u8 = 0;
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

/// Discover the CLI client config, rendering a `config_error` envelope on failure.
///
/// # Errors
/// Returns the CLI usage exit code when config discovery fails.
pub fn discover_config<WErr>(
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

pub fn render_help_command<WOut, WErr>(
    catalog: &CliOperationCatalogDocument,
    operation_argv: &[String],
    program: &str,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let rendered = match operation_argv {
        [] => Ok(render_catalog_help(catalog, program)),
        [operation] => render_operation_help(catalog, operation, program),
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

#[must_use]
pub fn operation_requires_args(catalog: &CliOperationCatalogDocument, operation: &str) -> bool {
    catalog
        .operations
        .iter()
        .find(|spec| spec.name == operation)
        .is_some_and(|spec| spec.args.iter().any(|arg| arg.required))
}

pub fn take_progress_flag(argv: &mut Vec<String>) -> bool {
    let before = argv.len();
    argv.retain(|arg| arg != "--progress");
    argv.len() != before
}

pub async fn run_request_from_catalog<WOut, WErr>(
    client: &GatewayClient,
    request_input: BuildRequestInput,
    catalog: &CliOperationCatalogDocument,
    stream_logs: bool,
    stdout: &mut WOut,
    stderr: &mut WErr,
) -> u8
where
    WOut: Write,
    WErr: Write,
{
    let started = Instant::now();
    let request = match build_request_from_catalog(request_input, catalog) {
        Ok(request) => request,
        Err(error) => {
            let _ = render_request_error(&error, stderr);
            return EXIT_USAGE;
        }
    };

    let response = match client
        .send_with_logs(&request, stream_logs, |log| {
            let _ = cli_log(stderr, started, log);
        })
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = render_error(error.kind(), error.to_string(), stderr);
            return EXIT_FAILURE;
        }
    };

    if stream_logs && response.get("error").is_none() {
        let _ = writeln!(stderr, "[Output]");
    }
    render_response(&response, stdout, stderr).unwrap_or(EXIT_FAILURE)
}

/// Render a response: success JSON to stdout (exit 0), error envelope to stderr
/// (exit 1).
///
/// # Errors
/// Returns an I/O error when writing the JSON line fails.
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

/// Render an error envelope with the given kind and message to stderr.
///
/// # Errors
/// Returns an I/O error when writing the JSON line fails.
pub fn render_error<WErr>(
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

/// Render a request-build error as an `invalid_request` envelope to stderr.
///
/// # Errors
/// Returns an I/O error when writing the JSON line fails.
pub fn render_request_error<WErr>(error: &RequestBuildError, stderr: &mut WErr) -> io::Result<()>
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

fn cli_log<W>(writer: &mut W, started: Instant, message: &str) -> io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "[progress {:.3}s] {message}",
        started.elapsed().as_secs_f64()
    )
}

fn json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    line
}
