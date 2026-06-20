//! Shared sandbox RPC protocol primitives.
//!
//! This crate defines generic request and response types plus protocol-neutral
//! operation metadata. It does not open sockets, dispatch operations, or know
//! command/workspace semantics.

#![forbid(unsafe_code)]

pub mod auth;
pub mod catalog;
pub mod error_kind;
mod framing;
pub mod limits;
pub mod manual;
pub mod operation_spec;
pub mod request;
pub mod response;
pub mod scope;

pub use auth::DAEMON_AUTH_FIELD;
pub use catalog::{OperationAuthority, OperationCatalog};
pub use limits::{MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S};
pub use operation_spec::{ArgCliSpec, ArgKind, ArgSpec, CliSpec, OperationFamily, OperationSpec};
pub use request::{
    decode_request_object, ArgsPresence, OwnedRequest, Request, RpcRequest, SandboxRequest,
};
pub use response::{
    error_response_with_details, response_line, Response, ResponseError, ResponseMeta,
    ResponseStatus, SandboxResponse,
};
pub use scope::OperationScope;
