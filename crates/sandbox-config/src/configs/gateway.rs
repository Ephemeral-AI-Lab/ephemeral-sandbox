//! Typed schema for the optional `gateway` section of the sandbox config,
//! doubling as the gateway server's runtime config.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use serde::Deserialize;
use thiserror::Error;

use crate::configs::validate::{require_non_empty, require_usize_at_least, ConfigFieldError};

#[cfg(windows)]
pub const DEFAULT_GATEWAY_SOCKET: &str = "npipe://./pipe/ephemeral-sandbox-gateway";
#[cfg(not(windows))]
pub const DEFAULT_GATEWAY_SOCKET: &str = "127.0.0.1:7878";
pub const DEFAULT_GATEWAY_PID: &str = "/tmp/eos-gateway.pid";
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 256;
pub const SANDBOX_GATEWAY_SOCKET_ENV: &str = "SANDBOX_GATEWAY_SOCKET";
pub const SANDBOX_GATEWAY_AUTH_TOKEN_ENV: &str = "SANDBOX_GATEWAY_AUTH_TOKEN";

/// A validated gateway transport endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEndpoint {
    Tcp(SocketAddr),
    WindowsNamedPipe(String),
    UnixSocket(PathBuf),
}

impl FromStr for GatewayEndpoint {
    type Err = GatewayEndpointParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value {
            return Err(GatewayEndpointParseError::new(
                "surrounding whitespace is not allowed",
            ));
        }
        if let Some(address) = value.strip_prefix("tcp://") {
            return parse_tcp(address);
        }
        if let Some(name) = value.strip_prefix("npipe://./pipe/") {
            return parse_named_pipe(name);
        }
        if let Some(path) = value.strip_prefix("unix://") {
            return parse_unix_socket(path);
        }
        if value.contains("://") {
            return Err(GatewayEndpointParseError::new(
                "supported schemes are tcp, npipe, and unix",
            ));
        }
        parse_tcp(value)
    }
}

impl std::fmt::Display for GatewayEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(address) => write!(formatter, "tcp://{address}"),
            Self::WindowsNamedPipe(path) => {
                let name = path
                    .strip_prefix(r"\\.\pipe\")
                    .unwrap_or(path)
                    .replace('\\', "/");
                write!(formatter, "npipe://./pipe/{name}")
            }
            Self::UnixSocket(path) => write!(formatter, "unix://{}", path.display()),
        }
    }
}

/// An invalid gateway endpoint string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid gateway endpoint: {reason}")]
pub struct GatewayEndpointParseError {
    reason: String,
}

impl GatewayEndpointParseError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Gateway server config. The YAML `gateway` section feeds `bind_addr`,
/// `pid_path`, and `max_concurrent_connections`; the auth token is runtime
/// state resolved from flag/env only and never deserializes from YAML.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    pub bind_addr: String,
    pub pid_path: PathBuf,
    pub max_concurrent_connections: usize,
    #[serde(skip)]
    pub auth_token: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_GATEWAY_SOCKET.to_owned(),
            pid_path: PathBuf::from(DEFAULT_GATEWAY_PID),
            max_concurrent_connections: DEFAULT_MAX_CONCURRENT_CONNECTIONS,
            auth_token: None,
        }
    }
}

impl GatewayConfig {
    #[must_use]
    pub fn new(
        bind_addr: impl Into<String>,
        pid_path: impl Into<PathBuf>,
        max_concurrent_connections: usize,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            bind_addr: bind_addr.into(),
            pid_path: pid_path.into(),
            max_concurrent_connections,
            auth_token,
        }
    }

    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates gateway policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        self.endpoint()
            .map_err(|error| ConfigFieldError::new("gateway.bind_addr", error.to_string()))?;
        require_non_empty(&self.pid_path.to_string_lossy(), "gateway.pid_path")?;
        require_usize_at_least(
            self.max_concurrent_connections,
            1,
            "gateway.max_concurrent_connections",
        )
    }

    /// Parse the configured bind value into its transport-specific endpoint.
    ///
    /// # Errors
    /// Returns an error when the endpoint syntax or transport value is unsafe.
    pub fn endpoint(&self) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
        self.bind_addr.parse()
    }
}

fn parse_tcp(value: &str) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    let address = value.parse::<SocketAddr>().map_err(|_| {
        GatewayEndpointParseError::new(format!("`{value}` must be an IP host:port address"))
    })?;
    if address.port() == 0 {
        return Err(GatewayEndpointParseError::new(
            "TCP port must be greater than zero",
        ));
    }
    Ok(GatewayEndpoint::Tcp(address))
}

fn parse_named_pipe(value: &str) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    validate_endpoint_segments(value, "named-pipe name")?;
    if value.chars().any(|character| {
        !character.is_ascii_alphanumeric() && !matches!(character, '/' | '-' | '_' | '.')
    }) {
        return Err(GatewayEndpointParseError::new(
            "named-pipe name may contain only ASCII letters, digits, `/`, `-`, `_`, and `.`",
        ));
    }
    let path = format!(r"\\.\pipe\{}", value.replace('/', r"\"));
    if path.encode_utf16().count() > 256 {
        return Err(GatewayEndpointParseError::new(
            "named-pipe path exceeds the Windows 256-character limit",
        ));
    }
    Ok(GatewayEndpoint::WindowsNamedPipe(path))
}

fn parse_unix_socket(value: &str) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    if !value.starts_with('/') {
        return Err(GatewayEndpointParseError::new(
            "Unix-socket path must be absolute",
        ));
    }
    let relative = value.trim_start_matches('/');
    validate_endpoint_segments(relative, "Unix-socket path")?;
    if value.len() > 100 {
        return Err(GatewayEndpointParseError::new(
            "Unix-socket path exceeds the portable 100-byte limit",
        ));
    }
    if value.chars().any(char::is_control) || value.contains('\\') {
        return Err(GatewayEndpointParseError::new(
            "Unix-socket path contains an unsafe character",
        ));
    }
    Ok(GatewayEndpoint::UnixSocket(PathBuf::from(value)))
}

fn validate_endpoint_segments(
    value: &str,
    description: &str,
) -> Result<(), GatewayEndpointParseError> {
    if value.is_empty()
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(GatewayEndpointParseError::new(format!(
            "{description} must contain non-empty path segments without `.` or `..`"
        )));
    }
    Ok(())
}
