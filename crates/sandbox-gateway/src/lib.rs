//! Authenticated gateway transport and the concrete manager composition root.
#![forbid(unsafe_code)]

mod daemon_client;
pub mod gateway;
mod local_daemon_installer;

pub use daemon_client::TcpSandboxDaemonClient;
pub use gateway::{
    resolve_gateway_config, GatewayCliOverrides, GatewayConfig, GatewayEndpoint,
    GatewayEndpointParseError, GatewayError, SandboxGatewayServer, DEFAULT_GATEWAY_PID,
    DEFAULT_GATEWAY_SOCKET, DEFAULT_MAX_CONCURRENT_CONNECTIONS, SANDBOX_GATEWAY_AUTH_TOKEN_ENV,
    SANDBOX_GATEWAY_SOCKET_ENV,
};
pub use local_daemon_installer::{LocalDaemonTimeouts, LocalSandboxDaemonInstaller};
