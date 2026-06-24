use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};

/// The black-box driver binary name; the sole CLI every code path shells out to.
pub const CLI_BIN: &str = "sandbox-cli";

/// One captured `sandbox-cli` invocation. `request_json` is `None` on the
/// black-box path because the CLI never echoes the wire request to stdio (it is
/// written only to the socket); the field exists for parity with the parent
/// record and future request-constructing callers.
#[derive(Clone)]
pub struct CallRecord {
    pub argv: Vec<String>,
    pub request_json: Option<Value>,
    pub response_json: Value,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub latency_ms: u128,
}

/// Drives the `sandbox-cli` wrapper over the public gateway socket boundary.
pub struct CliClient {
    cli_path: PathBuf,
    gateway_socket: PathBuf,
}

impl CliClient {
    #[must_use]
    pub fn new(cli_path: PathBuf, gateway_socket: PathBuf) -> Self {
        Self {
            cli_path,
            gateway_socket,
        }
    }

    /// Run `sandbox-cli manager <operation> <args...>` and capture the record.
    pub fn manager(&self, operation: &str, args: &[&str]) -> CallRecord {
        let mut subcommand = vec!["manager".to_owned(), operation.to_owned()];
        subcommand.extend(args.iter().copied().map(str::to_owned));
        self.invoke(subcommand)
    }

    /// Run `sandbox-cli runtime --sandbox-id <id> <operation> <args...>`.
    pub fn runtime(&self, sandbox_id: &str, operation: &str, args: &[&str]) -> CallRecord {
        let mut subcommand = vec![
            "runtime".to_owned(),
            "--sandbox-id".to_owned(),
            sandbox_id.to_owned(),
            operation.to_owned(),
        ];
        subcommand.extend(args.iter().copied().map(str::to_owned));
        self.invoke(subcommand)
    }

    fn invoke(&self, subcommand: Vec<String>) -> CallRecord {
        let mut argv = vec![
            "--gateway-socket".to_owned(),
            self.gateway_socket.to_string_lossy().into_owned(),
        ];
        argv.extend(subcommand);

        let started = Instant::now();
        let output = match Command::new(&self.cli_path).args(&argv).output() {
            Ok(output) => output,
            Err(error) => {
                return CallRecord {
                    argv,
                    request_json: None,
                    response_json: Value::Null,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("failed to spawn {}: {error}", self.cli_path.display()),
                    latency_ms: started.elapsed().as_millis(),
                };
            }
        };
        let latency_ms = started.elapsed().as_millis();

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let carrier: &[u8] = if exit_code == 0 {
            &output.stdout
        } else {
            &output.stderr
        };
        let response_json = serde_json::from_slice::<Value>(carrier).unwrap_or_default();

        CallRecord {
            argv,
            request_json: None,
            response_json,
            exit_code,
            stdout,
            stderr,
            latency_ms,
        }
    }
}

impl CallRecord {
    /// The parsed response is the bare result object (success) or
    /// `{ error: {..} }` (failure). On exit 0 the line came from stdout; on exit
    /// 1/2 it came from stderr. `response_json` is parsed from whichever stream
    /// carried the line. When `sandbox-cli` cannot be spawned the record carries
    /// `exit_code == -1`, a `Null` response, and the OS error in `stderr`, so
    /// callers can treat the spawn failure as non-fatal by inspecting `exit_code`.
    #[must_use]
    pub fn response(&self) -> &Value {
        &self.response_json
    }

    /// One `exchange.jsonl` row mapping this record 1:1: `argv`, the always-null
    /// black-box `request`, the parsed `response`, `exit_code`, both captured
    /// streams, and `latency_ms`. Centralizing the field names here keeps
    /// `report.rs` from duplicating them.
    #[must_use]
    pub fn to_exchange_line(&self) -> Value {
        json!({
            "argv": self.argv,
            "request": self.request_json,
            "response": self.response_json,
            "exit_code": self.exit_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "latency_ms": self.latency_ms,
        })
    }
}
