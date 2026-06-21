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
pub mod operation_spec;
pub mod request;
pub mod response;
pub mod scope;

pub use auth::DAEMON_AUTH_FIELD;
pub use catalog::{
    catalog_from_value, catalog_to_value, operation_execution_space_name, ArgCliSpecDocument,
    ArgSpecDocument, CatalogDecodeError, CliSpecDocument, OperationCatalog,
    OperationCatalogDocument, OperationExecutionSpace, OperationSpecDocument,
};
pub use limits::{MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S};
pub use operation_spec::{ArgCliSpec, ArgKind, ArgSpec, CliSpec, OperationSpec};
pub use request::{decode_request_value, Request};
pub use response::{error_response_with_details, response_line, Response};
pub use scope::OperationScope;
