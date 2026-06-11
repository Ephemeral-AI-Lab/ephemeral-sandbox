//! Client-hop wire handling: newline-delimited compact JSON over a Unix
//! socket, one request per connection, plus the API error envelope.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// Maximum bytes in one request frame (mirrors the box hop).
pub const MAX_REQUEST_BYTES: usize = eos_sandbox_host::MAX_REQUEST_BYTES;

/// Per-request read timeout in seconds (mirrors the box hop).
pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// One decoded client request envelope (SPEC §3.1).
#[derive(Debug)]
pub struct ClientRequest {
    /// Canonical name or listed alias from the catalog.
    pub op: String,
    /// Present for daemon-bound ops; absent on `sandbox.acquire`/`sandbox.list`.
    pub sandbox_id: Option<String>,
    /// uuid4 hex; correlates cancellation/heartbeat.
    pub invocation_id: String,
    /// Op-specific args (always an object).
    pub args: Value,
}

/// A framing/decoding failure, carrying its wire error kind.
#[derive(Debug)]
pub struct WireError {
    /// Error kind string for the envelope.
    pub kind: &'static str,
    /// Human message for the envelope.
    pub message: String,
}

impl WireError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Read one newline-terminated request frame, enforcing the size cap.
///
/// # Errors
/// Returns `request_too_large` past the cap or `invalid_envelope` on I/O
/// failure / EOF without a frame.
pub fn read_request_line(stream: impl Read) -> Result<Vec<u8>, WireError> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES as u64 + 1));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .map_err(|err| WireError::new("invalid_envelope", format!("read request: {err}")))?;
    if line.is_empty() {
        return Err(WireError::new(
            "invalid_envelope",
            "connection closed before a request line",
        ));
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(WireError::new(
            "request_too_large",
            format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    Ok(line)
}

/// Decode one request envelope.
///
/// # Errors
/// Returns `bad_json` for undecodable bytes and `invalid_envelope` for a
/// well-formed object missing required fields.
pub fn parse_request(line: &[u8]) -> Result<ClientRequest, WireError> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|err| WireError::new("bad_json", format!("request is not valid JSON: {err}")))?;
    let Value::Object(mut object) = value else {
        return Err(WireError::new(
            "invalid_envelope",
            "request must be a JSON object",
        ));
    };
    let op = take_string(&mut object, "op")?;
    if op.trim().is_empty() {
        return Err(WireError::new("invalid_envelope", "op is required"));
    }
    let invocation_id = take_string(&mut object, "invocation_id")?;
    let sandbox_id = match object.remove("sandbox_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id),
        Some(_) => {
            return Err(WireError::new(
                "invalid_envelope",
                "sandbox_id must be a string",
            ))
        }
    };
    let args = object.remove("args").unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err(WireError::new("invalid_envelope", "args must be an object"));
    }
    Ok(ClientRequest {
        op,
        sandbox_id,
        invocation_id,
        args,
    })
}

fn take_string(object: &mut Map<String, Value>, field: &str) -> Result<String, WireError> {
    match object.remove(field) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(WireError::new(
            "invalid_envelope",
            format!("{field} is required and must be a string"),
        )),
    }
}

/// Build the API error envelope (same shape as the daemon's).
#[must_use]
pub fn error_envelope(kind: &str, message: &str) -> Value {
    json!({
        "success": false,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind,
            "message": message,
            "details": {},
        },
    })
}

/// Encode one response line (compact JSON + `\n`).
#[must_use]
pub fn response_line(response: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(response).unwrap_or_default();
    line.push(b'\n');
    line
}
