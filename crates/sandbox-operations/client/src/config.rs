//! Gateway client discovery from explicit overrides and environment variables.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::GatewayEndpoint;

pub const SANDBOX_GATEWAY_SOCKET_ENV: &str = "SANDBOX_GATEWAY_SOCKET";
pub const SANDBOX_GATEWAY_AUTH_TOKEN_ENV: &str = "SANDBOX_GATEWAY_AUTH_TOKEN";
#[cfg(windows)]
pub const DEFAULT_GATEWAY_SOCKET: &str = "npipe://./pipe/ephemeral-sandbox-gateway";
#[cfg(not(windows))]
pub const DEFAULT_GATEWAY_SOCKET: &str = "127.0.0.1:7878";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfig {
    pub gateway_socket_path: PathBuf,
    pub gateway_endpoint: GatewayEndpoint,
    pub gateway_auth_token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayConfigOverrides {
    pub gateway_socket_path: Option<PathBuf>,
    pub gateway_auth_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

impl GatewayConfig {
    /// Discover gateway client configuration from overrides and environment.
    ///
    /// # Errors
    /// Returns an error when a configured socket path or auth token is invalid.
    pub fn discover(overrides: GatewayConfigOverrides) -> Result<Self, ConfigError> {
        Self::discover_with(overrides, |key| std::env::var_os(key))
    }

    /// Discover gateway client configuration using an injected environment reader.
    ///
    /// # Errors
    /// Returns an error when a configured socket path or auth token is invalid.
    pub fn discover_with(
        overrides: GatewayConfigOverrides,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, ConfigError> {
        let env_gateway_socket = env(SANDBOX_GATEWAY_SOCKET_ENV).map(PathBuf::from);
        let env_gateway_auth_token = env(SANDBOX_GATEWAY_AUTH_TOKEN_ENV)
            .map(|value| value.to_string_lossy().into_owned())
            .map(non_empty_auth_token)
            .transpose()?;

        let gateway_socket_path = overrides
            .gateway_socket_path
            .or(env_gateway_socket)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GATEWAY_SOCKET));

        if gateway_socket_path.as_os_str().is_empty() {
            return Err(config_error("gateway socket path must be non-empty"));
        }
        let gateway_endpoint = GatewayEndpoint::parse(&gateway_socket_path.to_string_lossy())
            .map_err(|error| config_error(format!("invalid gateway endpoint: {error}")))?;

        let gateway_auth_token = overrides
            .gateway_auth_token
            .map(non_empty_auth_token)
            .transpose()?
            .or(env_gateway_auth_token);

        Ok(Self {
            gateway_socket_path,
            gateway_endpoint,
            gateway_auth_token,
        })
    }
}

fn non_empty_auth_token(value: String) -> Result<String, ConfigError> {
    if value.trim().is_empty() {
        Err(config_error("gateway auth token must be non-empty"))
    } else {
        Ok(value)
    }
}

fn config_error(message: impl Into<String>) -> ConfigError {
    ConfigError {
        message: message.into(),
    }
}
