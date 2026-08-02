use sandbox_operation_contract::OperationRequest;
use sandbox_protocol::GATEWAY_AUTH_FIELD;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::{GatewayEndpoint, GatewayEndpointParseError, MAX_REQUEST_BYTES};

#[derive(Debug)]
pub struct GatewayClient {
    endpoint: Result<GatewayEndpoint, GatewayEndpointParseError>,
    auth_token: Option<String>,
}

#[derive(Debug)]
pub enum GatewayClientError {
    Endpoint(GatewayEndpointParseError),
    Transport(std::io::Error),
    Protocol(String),
    Json(serde_json::Error),
}

impl GatewayClient {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            endpoint: GatewayEndpoint::parse(&endpoint.into()),
            auth_token,
        }
    }

    #[must_use]
    pub const fn from_endpoint(endpoint: GatewayEndpoint, auth_token: Option<String>) -> Self {
        Self {
            endpoint: Ok(endpoint),
            auth_token,
        }
    }

    pub async fn send(&self, request: &OperationRequest) -> Result<Value, GatewayClientError> {
        self.send_with_logs(request, false, |_| {}).await
    }

    pub async fn send_with_logs<F>(
        &self,
        request: &OperationRequest,
        stream_logs: bool,
        on_log: F,
    ) -> Result<Value, GatewayClientError>
    where
        F: FnMut(&str),
    {
        let request_line = request_line(request, self.auth_token.as_deref(), stream_logs)?;
        let endpoint = self
            .endpoint
            .as_ref()
            .map_err(|error| GatewayClientError::Endpoint(error.clone()))?;
        let mut stream = connect(endpoint)
            .await
            .map_err(GatewayClientError::Transport)?;
        stream
            .write_all(&request_line)
            .await
            .map_err(GatewayClientError::Transport)?;
        stream
            .shutdown()
            .await
            .map_err(GatewayClientError::Transport)?;
        if stream_logs {
            read_response_stream(stream, on_log).await
        } else {
            read_response_line(stream).await
        }
    }
}

impl GatewayClientError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Endpoint(_) | Self::Transport(_) => "connection_error",
            Self::Protocol(_) | Self::Json(_) => "protocol_error",
        }
    }
}

impl std::fmt::Display for GatewayClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Endpoint(error) => write!(formatter, "invalid gateway endpoint: {error}"),
            Self::Transport(error) => write!(formatter, "gateway connection failed: {error}"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Json(error) => write!(formatter, "gateway response json failed: {error}"),
        }
    }
}

impl std::error::Error for GatewayClientError {}

trait GatewayIo: AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

impl<T> GatewayIo for T where T: AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

type GatewayStream = Box<dyn GatewayIo>;

async fn connect(endpoint: &GatewayEndpoint) -> std::io::Result<GatewayStream> {
    match endpoint {
        GatewayEndpoint::Tcp(address) => TcpStream::connect(address)
            .await
            .map(|stream| Box::new(stream) as GatewayStream),
        GatewayEndpoint::TcpHost(address) => TcpStream::connect(address.as_str())
            .await
            .map(|stream| Box::new(stream) as GatewayStream),
        GatewayEndpoint::WindowsNamedPipe(path) => connect_windows_named_pipe(path).await,
        GatewayEndpoint::UnixSocket(path) => connect_unix_socket(path).await,
    }
}

#[cfg(windows)]
async fn connect_windows_named_pipe(path: &str) -> std::io::Result<GatewayStream> {
    use tokio::net::windows::named_pipe::ClientOptions;

    ClientOptions::new()
        .open(path)
        .map(|stream| Box::new(stream) as GatewayStream)
}

#[cfg(not(windows))]
async fn connect_windows_named_pipe(_path: &str) -> std::io::Result<GatewayStream> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows named-pipe gateway endpoints are unavailable on this platform",
    ))
}

#[cfg(unix)]
async fn connect_unix_socket(path: &std::path::Path) -> std::io::Result<GatewayStream> {
    tokio::net::UnixStream::connect(path)
        .await
        .map(|stream| Box::new(stream) as GatewayStream)
}

#[cfg(not(unix))]
async fn connect_unix_socket(_path: &std::path::Path) -> std::io::Result<GatewayStream> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Unix-domain gateway endpoints are unavailable on this platform",
    ))
}

fn request_line(
    request: &OperationRequest,
    auth_token: Option<&str>,
    stream_logs: bool,
) -> Result<Vec<u8>, GatewayClientError> {
    let mut request_value = serde_json::to_value(request).map_err(GatewayClientError::Json)?;
    if let Value::Object(map) = &mut request_value {
        if let Some(token) = auth_token {
            map.insert(
                GATEWAY_AUTH_FIELD.to_owned(),
                Value::String(token.to_owned()),
            );
        }
        map.insert("_stream_logs".to_owned(), Value::Bool(stream_logs));
    }
    let mut line = serde_json::to_vec(&request_value).map_err(GatewayClientError::Json)?;
    line.push(b'\n');
    if line.len() > MAX_REQUEST_BYTES {
        return Err(GatewayClientError::Protocol(format!(
            "gateway request exceeded {MAX_REQUEST_BYTES} bytes"
        )));
    }
    Ok(line)
}

async fn read_response_line<S>(stream: S) -> Result<Value, GatewayClientError>
where
    S: AsyncRead + Unpin,
{
    let limit = u64::try_from(MAX_REQUEST_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reader = BufReader::new(stream.take(limit));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(GatewayClientError::Transport)?;
    validate_response_line(&line)?;
    serde_json::from_slice::<Value>(&line).map_err(GatewayClientError::Json)
}

async fn read_response_stream<S, F>(stream: S, mut on_log: F) -> Result<Value, GatewayClientError>
where
    S: AsyncRead + Unpin,
    F: FnMut(&str),
{
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(GatewayClientError::Transport)?;
        if line.is_empty() {
            return Err(GatewayClientError::Protocol(
                "gateway closed before returning a final response".to_owned(),
            ));
        }
        validate_response_line(&line)?;
        if let Some(log) = parse_cli_log_line(&line)? {
            on_log(&log);
            continue;
        }
        return serde_json::from_slice::<Value>(&line).map_err(GatewayClientError::Json);
    }
}

fn validate_response_line(line: &[u8]) -> Result<(), GatewayClientError> {
    if line.is_empty() {
        return Err(GatewayClientError::Protocol(
            "gateway returned an empty response".to_owned(),
        ));
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(GatewayClientError::Protocol(format!(
            "gateway response exceeded {MAX_REQUEST_BYTES} bytes"
        )));
    }
    if !line.ends_with(b"\n") {
        return Err(GatewayClientError::Protocol(
            "gateway response was not newline terminated".to_owned(),
        ));
    }
    Ok(())
}

fn parse_cli_log_line(line: &[u8]) -> Result<Option<String>, GatewayClientError> {
    let prefix = b"cli_log(";
    if !line.starts_with(prefix) {
        return Ok(None);
    }
    if !line.ends_with(b")\n") {
        return Err(GatewayClientError::Protocol(
            "gateway cli_log line was not terminated".to_owned(),
        ));
    }
    serde_json::from_slice(&line[prefix.len()..line.len() - 2])
        .map(Some)
        .map_err(GatewayClientError::Json)
}
