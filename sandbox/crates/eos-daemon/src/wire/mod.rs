//! The daemon wire protocol: message framing, frozen protocol constants, and
//! response canonicalization.
//!
//! This is in-box code. The host side carries its own copy of the vocabulary
//! (`eos-sandbox-host::protocol`); the shared artifact between them is
//! `crates/eos-operation/ops.json` plus `contract/` fixtures/prose, and drift
//! is caught by the conformance suites run by `cargo xtask check-contract`.

pub mod message;

pub use message::{
    decode, decode_value, encode, ErrorKind, ErrorResponse, ProtocolError, Request,
    RequestTraceContext, TraceLinkHint, WireMessage,
};

pub const DAEMON_PROTOCOL_VERSION: i64 = eos_operation::core::catalog::PROTOCOL_VERSION;
pub const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
pub const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
pub const CONNECT_FAILED: i32 = 97;
pub const IO_FAILED: i32 = 98;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const REQUEST_READ_TIMEOUT_S: f64 = 30.0;
pub const CONNECT_RETRY_DELAYS_S: [f64; 4] = [0.25, 0.5, 1.0, 2.0];
