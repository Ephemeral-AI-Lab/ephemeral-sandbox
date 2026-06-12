//! Newline-delimited JSON wire messages.
//!
//! Invariant: one compact JSON object per message + a single trailing `\n`
//! (`json.dumps(obj, separators=(",",":")) + "\n"`). [`encode`]/[`decode`] are
//! byte-stable for requests and error responses; responses are heterogeneous
//! `Value`s compared at the canonical bar (see [`super::canonical`]).
//!
//! The protocol-version field `_eos_daemon_protocol_version` lives INSIDE `args`
//! and the daemon NEVER reads it (an inert versioning hook). We reproduce its
//! presence but do not validate it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Encode/decode failures for the framed wire protocol. Distinct from the wire
/// [`ErrorKind`] (which is daemon policy, not a transport parse failure).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The request line was not valid UTF-8 JSON.
    #[error("bad json: {0}")]
    BadJson(#[from] serde_json::Error),
    /// The decoded value was not a JSON object.
    #[error("wire message must be a json object")]
    NotAnObject,
}

/// Request message (host -> daemon): `{op, invocation_id, args}`.
///
/// Field order on the wire is exactly this; top-level keys are not sorted by the
/// daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    pub invocation_id: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTraceContext {
    pub trace_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_hints: Vec<TraceLinkHint>,
    pub capture_budget_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceLinkHint {
    pub kind: String,
    pub value: String,
}

/// Daemon error response (`success:false`). `warnings`/`timings` are always
/// `[]`/`{}` at the builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub success: bool,
    #[serde(default)]
    pub warnings: Vec<Value>,
    #[serde(default)]
    pub timings: serde_json::Map<String, Value>,
    pub error: ErrorBody,
}

/// The `error` body of an [`ErrorResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}

/// Verified daemon error `kind` values. Serialized `snake_case` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// `op` missing/non-string/empty, or `args` present but not a dict.
    InvalidRequest,
    /// Request line was not valid UTF-8 JSON.
    BadJson,
    /// Request line exceeded `MAX_REQUEST_BYTES`.
    RequestTooLarge,
    /// TCP only: configured auth token did not match.
    Unauthorized,
    /// `op` not registered in the daemon op table.
    UnknownOp,
    /// A handler raised; `details.error_id` carries a uuid4 hex.
    InternalError,
    /// Operation/gate policy refusal.
    Forbidden,
    /// Refused because an isolated workspace is active for this agent.
    ForbiddenInIsolatedWorkspace,
    /// Refused because a lifecycle operation is in progress.
    LifecycleInProgress,
}

/// A framed wire message: a request, an error response, or any response
/// `Value`. Untagged: a request has `op`; an error has `success:false` + `error`;
/// any other object is a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireMessage {
    /// Host -> daemon request.
    Request(Request),
    /// Daemon -> host error response.
    Error(ErrorResponse),
    /// Daemon -> host response (heterogeneous; compared canonically).
    Response(Value),
}

/// Serialize a wire message as compact JSON plus a single trailing `\n`.
///
/// `serde_json` compact formatting matches the daemon for these ASCII payloads;
/// `args` key order is preserved (the `preserve_order` feature is required).
///
/// # Errors
///
/// Returns [`ProtocolError::BadJson`] when serde cannot serialize the message.
pub fn encode(message: &WireMessage) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decode one framed message. A trailing `\n` (and surrounding whitespace) is
/// tolerated; the body must be a single JSON object.
///
/// # Errors
///
/// Returns [`ProtocolError::BadJson`] for invalid JSON and
/// [`ProtocolError::NotAnObject`] when the decoded value is not a JSON object.
pub fn decode(bytes: &[u8]) -> Result<WireMessage, ProtocolError> {
    decode_value(serde_json::from_slice(bytes)?)
}

/// Disambiguate an already-parsed JSON value into an [`WireMessage`].
///
/// Lets a caller that already holds a [`Value`] (e.g. after stripping a
/// transport auth field) avoid re-serializing and re-parsing the payload.
///
/// # Errors
///
/// Returns [`ProtocolError::NotAnObject`] when `value` is not a JSON object, or
/// [`ProtocolError::BadJson`] when a request/error response fails to deserialize.
pub fn decode_value(value: Value) -> Result<WireMessage, ProtocolError> {
    // Disambiguate so a request never deserializes as a bare `Response(Value)`.
    let Some(obj) = value.as_object() else {
        return Err(ProtocolError::NotAnObject);
    };
    if obj.contains_key("op") {
        let req: Request = serde_json::from_value(value)?;
        return Ok(WireMessage::Request(req));
    }
    if obj.get("success") == Some(&Value::Bool(false)) && obj.contains_key("error") {
        let err: ErrorResponse = serde_json::from_value(value)?;
        return Ok(WireMessage::Error(err));
    }
    Ok(WireMessage::Response(value))
}

#[cfg(test)]
#[path = "../../tests/unit/wire/message.rs"]
mod tests;
