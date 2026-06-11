//! Host-side daemon protocol client and duplicated wire vocabulary.
//!
//! Speaks the raw daemon wire: one compact JSON object per line with a single
//! trailing `\n`. TCP transport with the top-level `_eos_daemon_auth_token`
//! envelope key (popped by the daemon before dispatch). One short-lived
//! connection per call keeps concurrency trivial (N clients = N connections)
//! and is robust to the daemon's one-request-per-connection handling.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Map, Value};

/// Top-level envelope key carrying the TCP auth token; popped by the daemon
/// before dispatch.
pub const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";

/// Args key carrying the protocol version (currently inert daemon-side).
pub const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";

/// Protocol version stamped into `args` by the host.
pub const DAEMON_PROTOCOL_VERSION: i64 = 1;

/// Maximum bytes in one request frame, both hops.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// Backoff between connect retries after a failure, then one final attempt
/// (inherited from the frozen host behavior).
pub const CONNECT_RETRY_DELAYS_S: [f64; 4] = [0.25, 0.5, 1.0, 2.0];

/// The liveness op. The bring-up ready gate polls it until the daemon answers
/// with success: `sandbox.runtime.ready` cannot gate provisioning because its
/// `control_plane` probe only turns `ready: true` once a workspace base
/// exists, and provisioning seeds none.
pub const HEARTBEAT_OP: &str = "sandbox.call.heartbeat";

/// The readiness probe op. Requires a `layer_stack_root` arg; used for status
/// embedding and recovery diagnostics, not the provision gate.
pub const READY_OP: &str = "sandbox.runtime.ready";

/// The conventional in-box layer-stack root the host stamps when a request
/// carries none (and the root the status readiness probe reports against).
pub const DEFAULT_LAYER_STACK_ROOT: &str = "/eos/layer-stack";

/// A transport-level client failure. Daemon error envelopes are NOT errors —
/// they come back as decoded success values.
///
/// The variants are load-bearing for the recovery ladder: a [`Self::Connect`]
/// failure invalidates the cached endpoint, while an ambiguous mid-request
/// failure on a mutating op must fail closed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// TCP connect failed (refused/reset/timeout) — the endpoint is suspect.
    #[error("connect {addr}: {source}")]
    Connect {
        /// The endpoint that refused the connection.
        addr: SocketAddr,
        /// Underlying socket error.
        #[source]
        source: std::io::Error,
    },
    /// Write/read failed after the connection was established.
    #[error("request i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The daemon half-closed without sending a response line.
    #[error("daemon closed connection without a response")]
    EmptyResponse,
    /// The response line was not valid JSON.
    #[error("decode response {raw:?}: {source}")]
    Decode {
        /// Verbatim response text.
        raw: String,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

impl ClientError {
    /// Whether the failure happened before the request could have been
    /// delivered (safe to re-resolve the endpoint and retry, even mutating).
    #[must_use]
    pub const fn is_connect_failure(&self) -> bool {
        matches!(self, Self::Connect { .. })
    }
}

/// A thin synchronous client for one daemon endpoint.
#[derive(Debug, Clone)]
pub struct ProtocolClient {
    addr: SocketAddr,
    auth_token: Option<String>,
    timeout: Duration,
}

impl ProtocolClient {
    /// Construct a client for `addr`, attaching `auth_token` to every request.
    #[must_use]
    pub fn new(addr: SocketAddr, auth_token: Option<String>, timeout: Duration) -> Self {
        Self {
            addr,
            auth_token,
            timeout,
        }
    }

    /// A clone of this client with a different auth token (for auth tests).
    #[must_use]
    pub fn with_token(&self, auth_token: Option<String>) -> Self {
        Self {
            addr: self.addr,
            auth_token,
            timeout: self.timeout,
        }
    }

    /// The endpoint this client talks to.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Issue one op with the client's configured auth token, stamping the
    /// protocol version and `invocation_id` into `args` when absent.
    ///
    /// Returns the decoded response `Value` (a success payload OR a daemon
    /// error envelope `{success:false, error:{kind,...}}`). Only transport/IO
    /// failures surface as `Err`.
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Value, ClientError> {
        let mut line = stamped_envelope_bytes(op, invocation_id, args, self.auth_token.as_deref());
        line.push(b'\n');
        self.request_raw(&line)
    }

    /// Issue one op with `args` passed through verbatim (no protocol-version or
    /// `invocation_id` stamping) — the shape the frozen readiness fixture pins.
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request_unstamped(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Result<Value, ClientError> {
        let mut line = raw_envelope_bytes(op, invocation_id, args, self.auth_token.as_deref());
        line.push(b'\n');
        self.request_raw(&line)
    }

    /// Send exact bytes (caller supplies framing) and read one response line.
    /// For malformed-frame and oversized-request contract tests.
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request_raw(&self, line: &[u8]) -> Result<Value, ClientError> {
        let mut stream =
            TcpStream::connect_timeout(&self.addr, self.timeout).map_err(|source| {
                ClientError::Connect {
                    addr: self.addr,
                    source,
                }
            })?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.set_nodelay(true).ok();
        stream.write_all(line)?;
        stream.flush().ok();

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        let read = reader.read_line(&mut response)?;
        if read == 0 {
            return Err(ClientError::EmptyResponse);
        }
        serde_json::from_str(response.trim_end()).map_err(|source| ClientError::Decode {
            raw: response,
            source,
        })
    }
}

/// Build the wire envelope bytes (no trailing newline) with host stamping:
/// `{op, invocation_id, args, _eos_daemon_auth_token?}` where the protocol
/// version and `invocation_id` are folded into `args` when the caller did not
/// set them.
#[must_use]
pub fn stamped_envelope_bytes(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Vec<u8> {
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
    raw_envelope_bytes(op, invocation_id, &Value::Object(args_obj), token)
}

/// Build the wire envelope bytes (no trailing newline) with `args` verbatim.
#[must_use]
pub fn raw_envelope_bytes(
    op: &str,
    invocation_id: &str,
    args: &Value,
    token: Option<&str>,
) -> Vec<u8> {
    let mut envelope = Map::new();
    envelope.insert("op".to_owned(), json!(op));
    envelope.insert("invocation_id".to_owned(), json!(invocation_id));
    envelope.insert("args".to_owned(), args.clone());
    if let Some(token) = token {
        envelope.insert(DAEMON_AUTH_FIELD.to_owned(), json!(token));
    }
    serde_json::to_vec(&Value::Object(envelope)).unwrap_or_default()
}

/// `true` when a decoded response is a success payload (`success != false`).
#[must_use]
pub fn is_success(response: &Value) -> bool {
    response.get("success") != Some(&Value::Bool(false))
}

/// The daemon error `kind` string, if `response` is an error envelope.
#[must_use]
pub fn error_kind(response: &Value) -> Option<&str> {
    response.get("error")?.get("kind")?.as_str()
}
