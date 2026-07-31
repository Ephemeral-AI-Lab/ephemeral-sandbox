use std::path::PathBuf;

pub use sandbox_config::configs::gateway::{
    GatewayConfig, GatewayEndpoint, GatewayEndpointParseError, DEFAULT_GATEWAY_PID,
    DEFAULT_GATEWAY_SOCKET, DEFAULT_MAX_CONCURRENT_CONNECTIONS, SANDBOX_GATEWAY_AUTH_TOKEN_ENV,
    SANDBOX_GATEWAY_SOCKET_ENV,
};

/// CLI-flag overrides for the gateway server; `None` means the flag was not
/// passed and the YAML `gateway` section (or its default) applies.
#[derive(Debug, Clone, Default)]
pub struct GatewayCliOverrides {
    pub bind_addr: Option<String>,
    pub pid_path: Option<PathBuf>,
    pub max_concurrent_connections: Option<usize>,
}

/// Resolve the effective gateway config with flag > env > YAML > default
/// precedence. `yaml` carries the loaded `gateway` section, or
/// `GatewayConfig::default()` when the document has none; the auth token is
/// resolved separately and never comes from YAML.
#[must_use]
pub fn resolve_gateway_config(
    overrides: GatewayCliOverrides,
    env_bind_addr: Option<String>,
    yaml: GatewayConfig,
) -> GatewayConfig {
    GatewayConfig {
        bind_addr: non_blank(overrides.bind_addr)
            .or_else(|| non_blank(env_bind_addr))
            .unwrap_or(yaml.bind_addr),
        pid_path: overrides.pid_path.unwrap_or(yaml.pid_path),
        max_concurrent_connections: overrides
            .max_concurrent_connections
            .unwrap_or(yaml.max_concurrent_connections),
        auth_token: yaml.auth_token,
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|addr| !addr.trim().is_empty())
}
