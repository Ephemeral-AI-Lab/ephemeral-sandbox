use sandbox_protocol::{Request, GATEWAY_AUTH_FIELD, MAX_REQUEST_BYTES};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::cli::timing;

const MAX_RESPONSE_BYTES: usize = MAX_REQUEST_BYTES;

#[derive(Debug)]
pub struct GatewayClient {
    addr: String,
    auth_token: Option<String>,
}

#[derive(Debug)]
pub enum GatewayClientError {
    Transport(std::io::Error),
    Protocol(String),
    Json(serde_json::Error),
}

impl GatewayClient {
    #[must_use]
    pub fn new(addr: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            addr: addr.into(),
            auth_token,
        }
    }

    pub async fn send(&self, request: &Request) -> Result<Value, GatewayClientError> {
        timing::checkpoint("client.send.start");
        let started = std::time::Instant::now();
        let connect_started = std::time::Instant::now();
        let mut stream = TcpStream::connect(self.addr.as_str())
            .await
            .map_err(GatewayClientError::Transport)?;
        timing::duration("client.connect", connect_started);
        let encode_started = std::time::Instant::now();
        let mut request_value = serde_json::to_value(request).map_err(GatewayClientError::Json)?;
        if let (Some(token), Value::Object(map)) = (&self.auth_token, &mut request_value) {
            map.insert(GATEWAY_AUTH_FIELD.to_owned(), Value::String(token.clone()));
        }
        let request_line = json_line(&request_value);
        timing::duration("client.encode_request", encode_started);
        let write_started = std::time::Instant::now();
        stream
            .write_all(&request_line)
            .await
            .map_err(GatewayClientError::Transport)?;
        stream
            .shutdown()
            .await
            .map_err(GatewayClientError::Transport)?;
        timing::duration("client.write_shutdown", write_started);
        let read_started = std::time::Instant::now();
        let response = read_response_line(stream).await;
        timing::duration("client.read_response", read_started);
        timing::duration("client.send.total", started);
        response
    }
}

impl GatewayClientError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Transport(_) => "connection_error",
            Self::Protocol(_) | Self::Json(_) => "protocol_error",
        }
    }
}

impl std::fmt::Display for GatewayClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "gateway connection failed: {error}"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Json(error) => write!(formatter, "gateway response json failed: {error}"),
        }
    }
}

impl std::error::Error for GatewayClientError {}

async fn read_response_line<S>(stream: S) -> Result<Value, GatewayClientError>
where
    S: AsyncRead + Unpin,
{
    let limit = u64::try_from(MAX_RESPONSE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reader = BufReader::new(stream.take(limit));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(GatewayClientError::Transport)?;
    if line.is_empty() {
        return Err(GatewayClientError::Protocol(
            "gateway returned an empty response".to_owned(),
        ));
    }
    if line.len() > MAX_RESPONSE_BYTES {
        return Err(GatewayClientError::Protocol(format!(
            "gateway response exceeded {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    if !line.ends_with(b"\n") {
        return Err(GatewayClientError::Protocol(
            "gateway response was not newline terminated".to_owned(),
        ));
    }
    serde_json::from_slice::<Value>(&line).map_err(GatewayClientError::Json)
}

fn json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    line
}
