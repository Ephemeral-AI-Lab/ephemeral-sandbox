//! Wire protocol, CAS byte-identity, and shared schema for the `eosd` runtime.
//!
//! Invariant: this crate is the source of truth and depends on nothing internal.
//! It owns the two correctness-bearing hashes (`manifest_root_hash`,
//! `layer_digest`) and the framed envelope encode/decode that must reproduce the
//! live Python byte-for-byte (at the AV-1c byte-identity bar for the CAS hashes
//! and request/error envelopes; the AV-1 canonical-equal bar for responses).
#![forbid(unsafe_code)]

pub mod audit;
pub mod canonical;
pub mod cas;
pub mod envelope;
pub mod models;
pub mod version;

pub use canonical::canonicalize;
pub use cas::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest,
};
pub use envelope::{decode, encode, Envelope, ErrorEnvelope, ErrorKind, ProtocolError, Request};
pub use models::{
    apply_search_replace, CommandOutput, ConflictInfo, EditFileArgs, EditFileResult,
    ExecCommandArgs, ExecCommandResult, GlobArgs, GlobResult, GrepArgs, GrepResult, Intent,
    PtyCancelArgs, PtyProgressArgs, PtyWriteArgs, ReadFileArgs, ReadFileResult, SearchReplaceEdit,
    SearchReplaceError, ShellArgs, ShellResult, WriteFileArgs, WriteFileResult,
};
pub use version::{
    CONNECT_FAILED, CONNECT_RETRY_DELAYS_S, DAEMON_AUTH_FIELD, DAEMON_PROTOCOL_FIELD,
    DAEMON_PROTOCOL_VERSION, IO_FAILED, MANIFEST_SCHEMA_VERSION, MAX_REQUEST_BYTES,
    REQUEST_READ_TIMEOUT_S,
};
