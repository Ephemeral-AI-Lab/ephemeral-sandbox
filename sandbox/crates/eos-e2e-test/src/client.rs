//! `ProtocolClient` — the ONLY egress to a live `eosd`.
//!
//! Speaks the raw `eos-protocol` wire: one compact JSON object per line with a
//! single trailing `\n`. TCP transport with the top-level `_eos_daemon_auth_token`
//! envelope key (popped by the daemon before dispatch). One short-lived
//! connection per call keeps concurrency trivial (N clients = N connections) and
//! is robust to the daemon's one-request-per-connection handling.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result};
use eos_protocol::{DAEMON_AUTH_FIELD, DAEMON_PROTOCOL_FIELD, DAEMON_PROTOCOL_VERSION};
use serde_json::{json, Map, Value};

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

    /// Issue one op with the client's configured auth token.
    ///
    /// Returns the decoded response `Value` (a success payload OR a daemon error
    /// envelope `{success:false, error:{kind,...}}`). Only transport/IO failures
    /// surface as `Err`.
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request(&self, op: &str, invocation_id: &str, args: &Value) -> Result<Value> {
        self.request_with_token(op, invocation_id, args, self.auth_token.as_deref())
    }

    /// Issue one op with an explicit auth token override (for auth tests).
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request_with_token(
        &self,
        op: &str,
        invocation_id: &str,
        args: &Value,
        token: Option<&str>,
    ) -> Result<Value> {
        let mut line = envelope_bytes(op, invocation_id, args, token);
        line.push(b'\n');
        self.request_raw(&line)
    }

    /// Send exact bytes (caller supplies framing) and read one response line.
    /// For malformed-frame and oversized-request contract tests.
    ///
    /// # Errors
    /// Returns an error on connect/write/read failure or undecodable response.
    pub fn request_raw(&self, line: &[u8]) -> Result<Value> {
        let mut stream = TcpStream::connect_timeout(&self.addr, self.timeout)
            .with_context(|| format!("connect {}", self.addr))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        stream.set_nodelay(true).ok();
        stream.write_all(line).context("write request")?;
        stream.flush().ok();

        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        let read = reader.read_line(&mut response).context("read response")?;
        if read == 0 {
            anyhow::bail!("daemon closed connection without a response");
        }
        serde_json::from_str(response.trim_end())
            .with_context(|| format!("decode response: {response:?}"))
    }
}

/// Build the wire envelope object bytes (no trailing newline):
/// `{op, invocation_id, args, _eos_daemon_auth_token?}` with the protocol
/// version folded into `args`.
#[must_use]
pub fn envelope_bytes(op: &str, invocation_id: &str, args: &Value, token: Option<&str>) -> Vec<u8> {
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

    let mut envelope = Map::new();
    envelope.insert("op".to_owned(), json!(op));
    envelope.insert("invocation_id".to_owned(), json!(invocation_id));
    envelope.insert("args".to_owned(), Value::Object(args_obj));
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
