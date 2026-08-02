//! Shared gateway transport, discovery, and value-based request construction.
#![forbid(unsafe_code)]

mod client;
mod config;
mod endpoint;
mod request;

pub use client::{GatewayClient, GatewayClientError};
pub use config::{
    ConfigError, GatewayConfig, GatewayConfigOverrides, DEFAULT_GATEWAY_SOCKET,
    SANDBOX_GATEWAY_AUTH_TOKEN_ENV, SANDBOX_GATEWAY_SOCKET_ENV,
};
pub use endpoint::{GatewayEndpoint, GatewayEndpointParseError};
pub use request::{
    build_request_from_values, build_request_from_values_with_id, catalog_arg_default,
    BuildRequestValueInput, RequestBuildError,
};

pub const MAX_REQUEST_BYTES: usize = sandbox_protocol::ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES;
