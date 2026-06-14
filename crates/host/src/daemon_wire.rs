use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Map, Value};

pub const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
pub const DAEMON_FORWARD_AUTH_FIELD: &str = "_eos_daemon_forward_auth_token";
pub const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
const DAEMON_TRACE_FIELD: &str = "trace";
pub const DAEMON_TRACE_SIDECAR_FIELD: &str = trace::TRACE_SIDECAR_FIELD;
pub const DAEMON_TRACE_SIDECAR_SCHEMA: &str = trace::TRACE_SIDECAR_SCHEMA;
pub const DAEMON_TRACE_SIDECAR_ENCODING: &str = trace::TRACE_SIDECAR_ENCODING;
pub const DAEMON_PROTOCOL_VERSION: i64 = 1;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
#[cfg(any(not(test), feature = "e2e-support"))]
pub const CONNECT_RETRY_DELAYS_S: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
pub(crate) const HEARTBEAT_OP: &str = ::protocol::catalog::SANDBOX_CALL_HEARTBEAT;
pub(crate) const READY_OP: &str = ::protocol::catalog::SANDBOX_RUNTIME_READY;
pub(crate) const DEFAULT_LAYER_STACK_ROOT: &str = "/eos/layer-stack";

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
    #[error("daemon response exceeds {limit} byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("decode response: {source} (raw_len={raw_len}, raw_sha256={raw_sha256})")]
    Decode {
        raw_len: usize,
        raw_sha256: String,
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
    forward_auth_token: Option<String>,
    timeout: Duration,
}

impl ProtocolClient {
    pub fn new(addr: SocketAddr, auth_token: Option<String>, timeout: Duration) -> Self {
        Self {
            addr,
            auth_token,
            forward_auth_token: None,
            timeout,
        }
    }

    pub fn new_forward_authorized(
        addr: SocketAddr,
        forward_auth_token: Option<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            addr,
            auth_token: None,
            forward_auth_token,
            timeout,
        }
    }

    #[cfg(feature = "e2e-support")]
    pub fn with_token(&self, auth_token: Option<String>) -> Self {
        Self {
            addr: self.addr,
            auth_token,
            forward_auth_token: None,
            timeout: self.timeout,
        }
    }

    #[cfg(feature = "e2e-support")]
    pub fn with_forward_token(&self, forward_auth_token: Option<String>) -> Self {
        Self {
            addr: self.addr,
            auth_token: None,
            forward_auth_token,
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
        let mut line = encode_request_with_auth(op, invocation_id, args, self.transport_auth());
        line.push(b'\n');
        self.request_raw(&line)
    }

    #[cfg(feature = "e2e-support")]
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
            self.transport_auth(),
            trace,
        );
        line.push(b'\n');
        self.request_raw(&line)
    }

    pub fn request_raw(&self, line: &[u8]) -> Result<Value, ClientError> {
        self.request_raw_observed(line)
            .map(|response| response.value)
    }

    pub(crate) fn request_raw_observed(
        &self,
        line: &[u8],
    ) -> Result<ProtocolResponse, ClientError> {
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
        let response = read_response_line(&mut reader)?;
        let value =
            serde_json::from_str(response.trim_end()).map_err(|source| ClientError::Decode {
                raw_len: response.len(),
                raw_sha256: trace::sha256_hex(response.as_bytes()),
                source,
            })?;
        Ok(ProtocolResponse {
            value,
            raw_bytes: response.into_bytes(),
        })
    }
}

fn read_response_line(reader: &mut impl BufRead) -> Result<String, ClientError> {
    read_response_line_with_limit(reader, MAX_RESPONSE_BYTES)
}

fn read_response_line_with_limit(
    reader: &mut impl BufRead,
    max_response_bytes: usize,
) -> Result<String, ClientError> {
    let limit = u64::try_from(max_response_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut limited = reader.take(limit);
    let mut response = String::new();
    let read = limited
        .read_line(&mut response)
        .map_err(ClientError::Read)?;
    if read == 0 {
        return Err(ClientError::EmptyResponse);
    }
    if response.len() > max_response_bytes {
        return Err(ClientError::ResponseTooLarge {
            limit: max_response_bytes,
        });
    }
    Ok(response)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProtocolResponse {
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

pub(crate) fn encode_request_with_trace_metadata(
    op: &str,
    invocation_id: &str,
    args: &Value,
    auth: TransportAuth<'_>,
    trace: &TraceWireContext,
) -> Vec<u8> {
    let stamped_args = stamped_args(args, invocation_id);
    let mut request = request_object(op, invocation_id, &Value::Object(stamped_args), auth);
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

#[cfg(any(test, feature = "e2e-support"))]
pub fn take_trace_sidecar_checked(
    response: &mut Value,
) -> Result<Option<Vec<u8>>, TraceSidecarError> {
    let present = response
        .as_object()
        .is_some_and(|object| object.contains_key(DAEMON_TRACE_SIDECAR_FIELD));
    let batch = decode_trace_sidecar_checked(response);
    if present {
        strip_trace_sidecar(response);
    }
    batch
}

pub fn decode_trace_sidecar_checked(
    response: &Value,
) -> Result<Option<Vec<u8>>, TraceSidecarError> {
    let Some(object) = response.as_object() else {
        return Ok(None);
    };
    let Some(sidecar) = object.get(DAEMON_TRACE_SIDECAR_FIELD) else {
        return Ok(None);
    };
    let encoded = trace_sidecar_payload(sidecar)?.to_owned();
    decode_trace_sidecar_base64(&encoded)
        .ok_or(TraceSidecarError::InvalidBase64)
        .map(Some)
}

fn trace_sidecar_payload(sidecar: &Value) -> Result<&str, TraceSidecarError> {
    match sidecar {
        Value::Object(object) => {
            if object.get("schema").and_then(Value::as_str) != Some(DAEMON_TRACE_SIDECAR_SCHEMA) {
                return Err(TraceSidecarError::InvalidEnvelope);
            }
            if object.get("encoding").and_then(Value::as_str) != Some(DAEMON_TRACE_SIDECAR_ENCODING)
            {
                return Err(TraceSidecarError::InvalidEnvelope);
            }
            if !object.get("spool_pending").is_some_and(Value::is_boolean) {
                return Err(TraceSidecarError::InvalidEnvelope);
            }
            object
                .get("data")
                .and_then(Value::as_str)
                .ok_or(TraceSidecarError::InvalidEnvelope)
        }
        _ => Err(TraceSidecarError::NonString),
    }
}

pub fn decode_trace_sidecar_base64(encoded: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSidecarError {
    NonString,
    InvalidBase64,
    InvalidEnvelope,
}

impl TraceSidecarError {
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::NonString => "non_string_sidecar",
            Self::InvalidBase64 => "invalid_base64",
            Self::InvalidEnvelope => "invalid_sidecar_envelope",
        }
    }
}

pub fn strip_trace_sidecar(response: &mut Value) {
    if let Some(object) = response.as_object_mut() {
        object.remove(DAEMON_TRACE_SIDECAR_FIELD);
    }
}

#[cfg(any(test, feature = "e2e-support"))]
pub fn encode_request_with_metadata(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Vec<u8> {
    encode_request_with_auth(op, invocation_id, args, TransportAuth::Raw(token))
}

pub(crate) fn encode_request_with_forward_metadata(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Vec<u8> {
    encode_request_with_auth(op, invocation_id, args, TransportAuth::Forward(token))
}

fn encode_request_with_auth(
    op: &str,
    invocation_id: &str,
    args: &Value,
    auth: TransportAuth<'_>,
) -> Vec<u8> {
    encode_request(
        op,
        invocation_id,
        &Value::Object(stamped_args(args, invocation_id)),
        auth,
    )
}

fn stamped_args(args: &Value, invocation_id: &str) -> Map<String, Value> {
    let mut args_obj = match args {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    args_obj.insert(
        DAEMON_PROTOCOL_FIELD.to_owned(),
        json!(DAEMON_PROTOCOL_VERSION),
    );
    args_obj
        .entry("invocation_id".to_owned())
        .or_insert_with(|| json!(invocation_id));
    args_obj
}
fn encode_request(op: &str, invocation_id: &str, args: &Value, auth: TransportAuth<'_>) -> Vec<u8> {
    serde_json::to_vec(&Value::Object(request_object(
        op,
        invocation_id,
        args,
        auth,
    )))
    .unwrap_or_default()
}

fn request_object(
    op: &str,
    invocation_id: &str,
    args: &Value,
    auth: TransportAuth<'_>,
) -> Map<String, Value> {
    let mut request = Map::new();
    request.insert("op".to_owned(), json!(op));
    request.insert("invocation_id".to_owned(), json!(invocation_id));
    request.insert("args".to_owned(), args.clone());
    match auth {
        TransportAuth::None => {}
        TransportAuth::Raw(Some(token)) => {
            request.insert(DAEMON_AUTH_FIELD.to_owned(), json!(token));
        }
        TransportAuth::Forward(Some(token)) => {
            request.insert(DAEMON_FORWARD_AUTH_FIELD.to_owned(), json!(token));
        }
        TransportAuth::Raw(None) | TransportAuth::Forward(None) => {}
    }
    request
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TransportAuth<'a> {
    None,
    Raw(Option<&'a str>),
    Forward(Option<&'a str>),
}

impl ProtocolClient {
    fn transport_auth(&self) -> TransportAuth<'_> {
        if let Some(token) = self.forward_auth_token.as_deref() {
            TransportAuth::Forward(Some(token))
        } else {
            TransportAuth::Raw(self.auth_token.as_deref())
        }
    }
}

pub fn response_envelope_status(response: &Value) -> &str {
    response
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| valid_response_status(status))
        .unwrap_or("error")
}

#[cfg(any(test, feature = "e2e-support"))]
pub fn response_domain_status(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
}

pub fn response_status(response: &Value) -> &str {
    response_envelope_status(response)
}

fn valid_response_status(status: &str) -> bool {
    matches!(
        status,
        "ok" | "running" | "rejected" | "cancelled" | "timed_out" | "error"
    )
}

pub fn response_fault_kind(response: &Value) -> Option<&str> {
    response
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
        .or_else(|| {
            (response.get("status").and_then(Value::as_str).is_none()).then_some("missing_status")
        })
}

pub fn response_is_accepted(response: &Value) -> bool {
    matches!(response_envelope_status(response), "ok" | "running")
}

#[cfg(test)]
mod tests {
    use super::{
        decode_trace_sidecar_base64, read_response_line_with_limit, response_domain_status,
        response_envelope_status, response_fault_kind, response_is_accepted, response_status,
        take_trace_sidecar_checked, ClientError, TraceSidecarError, DAEMON_TRACE_SIDECAR_ENCODING,
        DAEMON_TRACE_SIDECAR_SCHEMA,
    };
    use std::io::BufReader;

    use serde_json::json;

    #[test]
    fn reads_operation_envelope_statuses() {
        let cases = [
            (
                "envelope ok",
                json!({"status": "ok", "result": {"ready": true}, "meta": {}}),
                "ok",
                None,
                true,
                None,
            ),
            (
                "domain result running",
                json!({"status": "ok", "result": {"status": "running", "command_id": "cmd-1"}, "meta": {}}),
                "ok",
                Some("running"),
                true,
                None,
            ),
            (
                "envelope running",
                json!({"status": "running", "result": {"command_id": "cmd-1"}, "meta": {}}),
                "running",
                None,
                true,
                None,
            ),
            (
                "envelope error",
                json!({"status": "error", "error": {"kind": "internal_error", "message": "failed"}, "meta": {}}),
                "error",
                None,
                false,
                Some("internal_error"),
            ),
            (
                "envelope rejected without kind",
                json!({"status": "rejected", "error": {"message": "blocked"}, "meta": {}}),
                "rejected",
                None,
                false,
                None,
            ),
            (
                "invalid envelope status",
                json!({"status": "mystery", "meta": {}}),
                "error",
                None,
                false,
                None,
            ),
            (
                "missing envelope status",
                json!({"ready": true}),
                "error",
                None,
                false,
                Some("missing_status"),
            ),
        ];

        for (label, response, status, domain_status, accepted, kind) in cases {
            assert_eq!(
                response_status(&response),
                status,
                "{label}: response status"
            );
            assert_eq!(
                response_envelope_status(&response),
                status,
                "{label}: envelope status"
            );
            assert_eq!(
                response_domain_status(&response),
                domain_status,
                "{label}: domain status"
            );
            assert_eq!(
                response_is_accepted(&response),
                accepted,
                "{label}: accepted response"
            );
            assert_eq!(response_fault_kind(&response), kind, "{label}: fault kind");
        }
    }

    #[test]
    fn decodes_trace_sidecar_base64() {
        assert_eq!(
            decode_trace_sidecar_base64("AQID").as_deref(),
            Some(&[1, 2, 3][..])
        );
        assert!(decode_trace_sidecar_base64("not base64").is_none());
    }

    #[test]
    fn daemon_response_reads_are_bounded() {
        let mut reader = BufReader::new(&b"01234567890\n"[..]);
        let err = read_response_line_with_limit(&mut reader, 10)
            .expect_err("oversized daemon response rejected");

        assert!(
            matches!(err, ClientError::ResponseTooLarge { limit: 10 }),
            "{err:?}"
        );
    }

    #[test]
    fn checked_sidecar_decoder_strips_and_reports_malformed_values() {
        let mut wrapped = json!({
            "_trace_events": {
                "schema": DAEMON_TRACE_SIDECAR_SCHEMA,
                "encoding": DAEMON_TRACE_SIDECAR_ENCODING,
                "spool_pending": false,
                "data": "AQID",
            },
        });
        assert_eq!(
            take_trace_sidecar_checked(&mut wrapped)
                .expect("wrapped sidecar decodes")
                .as_deref(),
            Some(&[1, 2, 3][..])
        );
        assert!(wrapped.get("_trace_events").is_none());

        let mut bare_string = json!({"_trace_events": "AQID"});
        assert_eq!(
            take_trace_sidecar_checked(&mut bare_string),
            Err(TraceSidecarError::NonString)
        );
        assert!(bare_string.get("_trace_events").is_none());

        let mut invalid_base64 = json!({
            "_trace_events": {
                "schema": DAEMON_TRACE_SIDECAR_SCHEMA,
                "encoding": DAEMON_TRACE_SIDECAR_ENCODING,
                "spool_pending": false,
                "data": "not base64",
            },
        });
        assert_eq!(
            take_trace_sidecar_checked(&mut invalid_base64),
            Err(TraceSidecarError::InvalidBase64)
        );
        assert!(invalid_base64.get("_trace_events").is_none());

        let mut invalid_envelope = json!({"_trace_events": {"batch": "AQID"}});
        assert_eq!(
            take_trace_sidecar_checked(&mut invalid_envelope),
            Err(TraceSidecarError::InvalidEnvelope)
        );
        assert!(invalid_envelope.get("_trace_events").is_none());

        let mut non_string = json!({"_trace_events": 42});
        assert_eq!(
            take_trace_sidecar_checked(&mut non_string),
            Err(TraceSidecarError::NonString)
        );
        assert!(non_string.get("_trace_events").is_none());

        let mut absent = json!({"success": true});
        assert_eq!(take_trace_sidecar_checked(&mut absent), Ok(None));
    }
}
