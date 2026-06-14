//! The daemon wire protocol: message framing, frozen protocol constants, and
//! response canonicalization.
//!
//! This is in-box code. The shared vocabulary lives in the dependency-light
//! `protocol` crate, with owner-local protocol fixtures/prose catching
//! wire-format drift through the conformance suites run by
//! `cargo xtask check-contract`.

pub mod message;

pub use message::{
    decode, decode_value, encode, ErrorKind, ProtocolError, Request, RequestTraceContext,
    TraceLinkHint, WireMessage,
};

pub const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
pub const DAEMON_PROTOCOL_VERSION: i64 = 1;
pub const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
pub const DAEMON_FORWARD_AUTH_FIELD: &str = "_eos_daemon_forward_auth_token";
pub const CONNECT_FAILED: i32 = 97;
pub const IO_FAILED: i32 = 98;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const REQUEST_READ_TIMEOUT_S: f64 = 30.0;
