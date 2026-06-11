//! The daemon wire protocol: envelope framing, the op catalog (canonical
//! names), frozen protocol constants, and response canonicalization.
//!
//! This is in-box code. The host side carries its own copy of the vocabulary
//! (`eos-sandbox-host::protocol`); the shared artifact between them is
//! `contract/` (data + prose), and drift is caught by the conformance suites
//! run by `cargo xtask check-contract`.

pub mod canonical;
pub mod envelope;
pub mod ops;
pub mod version;

pub use canonical::canonicalize;
pub use envelope::{
    decode, decode_value, encode, Envelope, ErrorEnvelope, ErrorKind, ProtocolError, Request,
};
pub use version::{
    CONNECT_FAILED, CONNECT_RETRY_DELAYS_S, DAEMON_AUTH_FIELD, DAEMON_PROTOCOL_FIELD,
    DAEMON_PROTOCOL_VERSION, IO_FAILED, MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S,
};
