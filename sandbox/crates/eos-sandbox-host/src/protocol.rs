use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Map, Value};

pub const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
pub const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
pub const DAEMON_TRACE_FIELD: &str = "trace";
pub const DAEMON_TRACE_SIDECAR_FIELD: &str = "_trace_events";
pub const DAEMON_PROTOCOL_VERSION: i64 = 1;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const CONNECT_RETRY_DELAYS_S: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
pub const HEARTBEAT_OP: &str = "sandbox.call.heartbeat";
pub const READY_OP: &str = "sandbox.runtime.ready";
pub const DEFAULT_LAYER_STACK_ROOT: &str = "/eos/layer-stack";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connect {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("request i/o setup: {0}")]
    Io(std::io::Error),
    #[error("write request: {0}")]
    Write(#[source] std::io::Error),
    #[error("read response: {0}")]
    Read(#[source] std::io::Error),
    #[error("daemon closed connection without a response")]
    EmptyResponse,
    #[error("decode response {raw:?}: {source}")]
    Decode {
        raw: String,
        #[source]
        source: serde_json::Error,
    },
}

impl ClientError {
    pub(crate) const fn is_connect_failure(&self) -> bool {
        matches!(self, Self::Connect { .. })
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolClient {
    addr: SocketAddr,
    auth_token: Option<String>,
    timeout: Duration,
}

impl ProtocolClient {
    pub fn new(addr: SocketAddr, auth_token: Option<String>, timeout: Duration) -> Self {
        Self {
            addr,
            auth_token,
            timeout,
        }
    }

    pub fn with_token(&self, auth_token: Option<String>) -> Self {
        Self {
            addr: self.addr,
            auth_token,
            timeout: self.timeout,
        }
    }
    pub(crate) const fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn request(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Value, ClientError> {
        let mut line =
            encode_request_with_metadata(op, invocation_id, args, self.auth_token.as_deref());
        line.push(b'\n');
        self.request_raw(&line)
    }

    pub fn request_with_trace(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
        trace: &TraceWireContext,
    ) -> Result<Value, ClientError> {
        let mut line = encode_request_with_trace_metadata(
            op,
            invocation_id,
            args,
            self.auth_token.as_deref(),
            trace,
        );
        line.push(b'\n');
        self.request_raw(&line)
    }

    pub(crate) fn request_unstamped(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Value, ClientError> {
        let mut line = encode_request(op, invocation_id, args, self.auth_token.as_deref());
        line.push(b'\n');
        self.request_raw(&line)
    }

    pub fn request_raw(&self, line: &[u8]) -> Result<Value, ClientError> {
        self.request_raw_observed(line)
            .map(|response| response.value)
    }

    pub fn request_raw_observed(&self, line: &[u8]) -> Result<ProtocolResponse, ClientError> {
        let mut stream =
            TcpStream::connect_timeout(&self.addr, self.timeout).map_err(|source| {
                ClientError::Connect {
                    addr: self.addr,
                    source,
                }
            })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(ClientError::Io)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(ClientError::Io)?;
        stream.set_nodelay(true).ok();
        stream.write_all(line).map_err(ClientError::Write)?;
        stream.flush().ok();

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        let read = reader.read_line(&mut response).map_err(ClientError::Read)?;
        if read == 0 {
            return Err(ClientError::EmptyResponse);
        }
        let value =
            serde_json::from_str(response.trim_end()).map_err(|source| ClientError::Decode {
                raw: response.clone(),
                source,
            })?;
        Ok(ProtocolResponse {
            value,
            raw_bytes: response.into_bytes(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolResponse {
    pub value: Value,
    pub raw_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceWireContext {
    pub trace_id: String,
    pub request_id: String,
    pub parent_span_id: Option<u64>,
    pub link_hints: Vec<TraceWireLinkHint>,
    pub capture_budget_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceWireLinkHint {
    pub kind: String,
    pub value: String,
}

pub fn encode_request_with_trace_metadata(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
    trace: &TraceWireContext,
) -> Vec<u8> {
    let stamped_args = stamped_args(args, invocation_id);
    let mut request = request_object(op, invocation_id, &Value::Object(stamped_args), token);
    request.insert(
        DAEMON_TRACE_FIELD.to_owned(),
        json!({
            "trace_id": trace.trace_id,
            "request_id": trace.request_id,
            "parent_span_id": trace.parent_span_id,
            "link_hints": trace.link_hints.iter().map(|hint| {
                json!({"kind": hint.kind, "value": hint.value})
            }).collect::<Vec<_>>(),
            "capture_budget_version": trace.capture_budget_version,
        }),
    );
    serde_json::to_vec(&Value::Object(request)).unwrap_or_default()
}

pub fn take_trace_sidecar(response: &mut Value) -> Option<Vec<u8>> {
    let encoded = response
        .as_object_mut()?
        .remove(DAEMON_TRACE_SIDECAR_FIELD)?
        .as_str()
        .map(str::to_owned)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

pub fn strip_trace_sidecar(response: &mut Value) {
    if let Some(object) = response.as_object_mut() {
        object.remove(DAEMON_TRACE_SIDECAR_FIELD);
    }
}

pub fn encode_request_with_metadata(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Vec<u8> {
    encode_request(
        op,
        invocation_id,
        &Value::Object(stamped_args(args, invocation_id)),
        token,
    )
}

fn stamped_args(args: &Value, invocation_id: &str) -> Map<String, Value> {
    let mut args_obj = match args {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    args_obj
        .entry(DAEMON_PROTOCOL_FIELD.to_owned())
        .or_insert_with(|| json!(DAEMON_PROTOCOL_VERSION));
    args_obj
        .entry("invocation_id".to_owned())
        .or_insert_with(|| json!(invocation_id));
    args_obj
}
pub fn encode_request(op: &str, invocation_id: &str, args: &Value, token: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&Value::Object(request_object(
        op,
        invocation_id,
        args,
        token,
    )))
    .unwrap_or_default()
}

fn request_object(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Map<String, Value> {
    let mut request = Map::new();
    request.insert("op".to_owned(), json!(op));
    request.insert("invocation_id".to_owned(), json!(invocation_id));
    request.insert("args".to_owned(), args.clone());
    if let Some(token) = token {
        request.insert(DAEMON_AUTH_FIELD.to_owned(), json!(token));
    }
    request
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseShape {
    Envelope,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseClassification<'a> {
    pub shape: ResponseShape,
    pub status: &'a str,
    pub success: bool,
    pub error_kind: Option<&'a str>,
}

pub fn response_classification(response: &Value) -> ResponseClassification<'_> {
    if let Some(status) = response.get("status").and_then(Value::as_str) {
        return classify_envelope_response(response, status);
    }
    classify_legacy_response(response)
}

fn classify_envelope_response<'a>(
    response: &'a Value,
    status: &'a str,
) -> ResponseClassification<'a> {
    let error_kind = response
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str);
    match status {
        "ok" | "running" => ResponseClassification {
            shape: ResponseShape::Envelope,
            status,
            success: true,
            error_kind: None,
        },
        "rejected" | "cancelled" | "timed_out" | "error" => ResponseClassification {
            shape: ResponseShape::Envelope,
            status,
            success: false,
            error_kind: error_kind.or(Some(status)),
        },
        _ => ResponseClassification {
            shape: ResponseShape::Envelope,
            status: "error",
            success: false,
            error_kind: Some("invalid_status"),
        },
    }
}

fn classify_legacy_response(response: &Value) -> ResponseClassification<'_> {
    if response.get("success") == Some(&Value::Bool(false)) {
        return ResponseClassification {
            shape: ResponseShape::Legacy,
            status: "error",
            success: false,
            error_kind: response
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str),
        };
    }
    ResponseClassification {
        shape: ResponseShape::Legacy,
        status: "ok",
        success: true,
        error_kind: None,
    }
}

pub fn is_success(response: &Value) -> bool {
    response_classification(response).success
}
pub fn error_kind(response: &Value) -> Option<&str> {
    response_classification(response).error_kind
}

pub fn response_status(response: &Value) -> &str {
    response_classification(response).status
}

#[cfg(test)]
mod tests {
    use super::{error_kind, is_success, response_classification, ResponseShape};
    use serde_json::json;

    #[test]
    fn classifies_mixed_legacy_and_envelope_responses() {
        let cases = [
            (
                "legacy success",
                json!({"success": true, "ready": true}),
                ResponseShape::Legacy,
                "ok",
                true,
                None,
            ),
            (
                "legacy error",
                json!({"success": false, "error": {"kind": "bad_json", "message": "bad"}}),
                ResponseShape::Legacy,
                "error",
                false,
                Some("bad_json"),
            ),
            (
                "envelope ok",
                json!({"status": "ok", "result": {"ready": true}, "meta": {}}),
                ResponseShape::Envelope,
                "ok",
                true,
                None,
            ),
            (
                "envelope running",
                json!({"status": "running", "result": {"command_id": "cmd-1"}, "meta": {}}),
                ResponseShape::Envelope,
                "running",
                true,
                None,
            ),
            (
                "envelope error",
                json!({"status": "error", "error": {"kind": "internal_error", "message": "failed"}, "meta": {}}),
                ResponseShape::Envelope,
                "error",
                false,
                Some("internal_error"),
            ),
            (
                "envelope rejected without kind",
                json!({"status": "rejected", "error": {"message": "blocked"}, "meta": {}}),
                ResponseShape::Envelope,
                "rejected",
                false,
                Some("rejected"),
            ),
            (
                "invalid envelope status",
                json!({"status": "mystery", "meta": {}}),
                ResponseShape::Envelope,
                "error",
                false,
                Some("invalid_status"),
            ),
        ];

        for (label, response, shape, status, success, kind) in cases {
            let classification = response_classification(&response);
            assert_eq!(classification.shape, shape, "{label}: response shape");
            assert_eq!(classification.status, status, "{label}: response status");
            assert_eq!(classification.success, success, "{label}: success flag");
            assert_eq!(classification.error_kind, kind, "{label}: error kind");
            assert_eq!(is_success(&response), success, "{label}: helper success");
            assert_eq!(error_kind(&response), kind, "{label}: helper error kind");
        }
    }
}
