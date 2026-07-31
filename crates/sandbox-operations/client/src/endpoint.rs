use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

const TCP_SCHEME: &str = "tcp://";
const WINDOWS_NAMED_PIPE_SCHEME: &str = "npipe://./pipe/";
const WINDOWS_NAMED_PIPE_PREFIX: &str = r"\\.\pipe\";
const UNIX_SOCKET_SCHEME: &str = "unix://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEndpoint {
    Tcp(SocketAddr),
    WindowsNamedPipe(String),
    UnixSocket(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEndpointParseError {
    message: String,
}

impl GatewayEndpoint {
    /// Parse an explicit gateway endpoint URI or a legacy TCP socket address.
    ///
    /// # Errors
    /// Returns an error when the endpoint syntax, address, or path is invalid.
    pub fn parse(value: &str) -> Result<Self, GatewayEndpointParseError> {
        value.parse()
    }

    #[must_use]
    pub fn tcp(address: SocketAddr) -> Self {
        Self::Tcp(address)
    }

    /// Construct a gateway endpoint from a canonical Windows named-pipe path.
    ///
    /// # Errors
    /// Returns an error when the path is outside the local named-pipe namespace.
    pub fn windows_named_pipe(path: impl Into<String>) -> Result<Self, GatewayEndpointParseError> {
        let path = path.into();
        let Some(name) = path.strip_prefix(WINDOWS_NAMED_PIPE_PREFIX) else {
            return Err(endpoint_error(
                "Windows named-pipe path must start with \\\\.\\pipe\\",
            ));
        };
        parse_named_pipe_name(name, '\\')
    }

    /// Construct a gateway endpoint from an absolute Unix-domain socket path.
    ///
    /// # Errors
    /// Returns an error when the path is not absolute or has unsafe segments.
    pub fn unix_socket(path: impl Into<PathBuf>) -> Result<Self, GatewayEndpointParseError> {
        let path = path.into();
        let path_text = path.to_string_lossy();
        parse_unix_socket_path(&path_text)
    }
}

impl FromStr for GatewayEndpoint {
    type Err = GatewayEndpointParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(endpoint_error("gateway endpoint must be non-empty"));
        }
        if value.trim() != value {
            return Err(endpoint_error(
                "gateway endpoint must not contain surrounding whitespace",
            ));
        }
        if let Some(address) = value.strip_prefix(TCP_SCHEME) {
            return parse_tcp_address(address);
        }
        if let Some(name) = value.strip_prefix(WINDOWS_NAMED_PIPE_SCHEME) {
            return parse_named_pipe_name(name, '/');
        }
        if let Some(path) = value.strip_prefix(UNIX_SOCKET_SCHEME) {
            return parse_unix_socket_path(path);
        }
        if value.contains("://") {
            return Err(endpoint_error("unsupported gateway endpoint scheme"));
        }
        parse_tcp_address(value)
    }
}

impl std::fmt::Display for GatewayEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(address) => write!(formatter, "{TCP_SCHEME}{address}"),
            Self::WindowsNamedPipe(path) => {
                let name = path.strip_prefix(WINDOWS_NAMED_PIPE_PREFIX).unwrap_or(path);
                write!(
                    formatter,
                    "{WINDOWS_NAMED_PIPE_SCHEME}{}",
                    name.replace('\\', "/")
                )
            }
            Self::UnixSocket(path) => {
                write!(formatter, "{UNIX_SOCKET_SCHEME}{}", path.display())
            }
        }
    }
}

impl std::fmt::Display for GatewayEndpointParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GatewayEndpointParseError {}

fn parse_tcp_address(value: &str) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| endpoint_error("TCP gateway endpoint must be a valid IP socket address"))?;
    if address.port() == 0 {
        return Err(endpoint_error("TCP gateway endpoint port must be non-zero"));
    }
    Ok(GatewayEndpoint::Tcp(address))
}

fn parse_unix_socket_path(value: &str) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    let Some(name) = value.strip_prefix('/') else {
        return Err(endpoint_error("Unix-domain socket path must be absolute"));
    };
    validate_path_segments(name.split('/'), "Unix-domain socket")?;
    if value.len() > 100 {
        return Err(endpoint_error(
            "Unix-domain socket path exceeds the portable 100-byte limit",
        ));
    }
    if value.chars().any(char::is_control) || value.contains('\\') {
        return Err(endpoint_error(
            "Unix-domain socket path contains an unsafe character",
        ));
    }
    Ok(GatewayEndpoint::UnixSocket(PathBuf::from(value)))
}

fn parse_named_pipe_name(
    name: &str,
    separator: char,
) -> Result<GatewayEndpoint, GatewayEndpointParseError> {
    validate_path_segments(name.split(separator), "Windows named-pipe")?;
    if name.chars().any(|character| {
        !character.is_ascii_alphanumeric()
            && character != separator
            && !matches!(character, '-' | '_' | '.')
    }) {
        return Err(endpoint_error(
            "Windows named-pipe name contains an unsafe character",
        ));
    }
    let path = format!("{WINDOWS_NAMED_PIPE_PREFIX}{}", name.replace('/', "\\"));
    if path.encode_utf16().count() > 256 {
        return Err(endpoint_error(
            "Windows named-pipe path exceeds the 256-character limit",
        ));
    }
    Ok(GatewayEndpoint::WindowsNamedPipe(path))
}

fn validate_path_segments<'a>(
    segments: impl Iterator<Item = &'a str>,
    transport: &str,
) -> Result<(), GatewayEndpointParseError> {
    for segment in segments {
        if segment.is_empty() {
            return Err(endpoint_error(format!(
                "{transport} path must not contain empty segments"
            )));
        }
        if matches!(segment, "." | "..") {
            return Err(endpoint_error(format!(
                "{transport} path must not contain dot segments"
            )));
        }
    }
    Ok(())
}

fn endpoint_error(message: impl Into<String>) -> GatewayEndpointParseError {
    GatewayEndpointParseError {
        message: message.into(),
    }
}
