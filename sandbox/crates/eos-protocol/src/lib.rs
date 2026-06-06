//! Wire protocol, CAS byte-identity, and shared schema for the `eosd` runtime.
//!
//! Invariant: this crate is the source of truth and depends on nothing internal.
//! It owns the two correctness-bearing hashes (`manifest_root_hash`,
//! `layer_digest`) and the framed envelope encode/decode that must reproduce the
//! live Rust byte-for-byte (at the AV-1c byte-identity bar for the CAS hashes
//! and request/error envelopes; the AV-1 canonical-equal bar for responses).
#![forbid(unsafe_code)]

// Lib tests receive dev-dependencies used by fixture integration tests. Keep
// `unused_crate_dependencies` usable under `--all-targets` without an allow.
#[cfg(test)]
use base64 as _;

pub mod audit;
pub mod canonical;
pub mod cas;
pub mod envelope;
pub mod ids;
pub mod models;
pub mod ops;
pub mod version;

pub use canonical::canonicalize;
pub use cas::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest,
};
pub use envelope::{
    decode, decode_value, encode, Envelope, ErrorEnvelope, ErrorKind, ProtocolError, Request,
};
pub use ids::{CallerId, InvocationId, WorkspaceHandleId};
pub use models::Intent;
pub use version::{
    CONNECT_FAILED, CONNECT_RETRY_DELAYS_S, DAEMON_AUTH_FIELD, DAEMON_PROTOCOL_FIELD,
    DAEMON_PROTOCOL_VERSION, IO_FAILED, MANIFEST_SCHEMA_VERSION, MAX_REQUEST_BYTES,
    REQUEST_READ_TIMEOUT_S,
};
