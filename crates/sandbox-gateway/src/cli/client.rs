use sandbox_protocol::{Request, GATEWAY_AUTH_FIELD, MAX_REQUEST_BYTES};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

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
        self.send_with_events(request, false, |_| {}).await
    }

    pub async fn send_with_events<F>(
        &self,
        request: &Request,
        stream_events: bool,
        on_event: F,
    ) -> Result<Value, GatewayClientError>
    where
        F: FnMut(&Value),
    {
        let mut stream = TcpStream::connect(self.addr.as_str())
            .await
            .map_err(GatewayClientError::Transport)?;
        let mut request_value = serde_json::to_value(request).map_err(GatewayClientError::Json)?;
        if let (Some(token), Value::Object(map)) = (&self.auth_token, &mut request_value) {
            map.insert(GATEWAY_AUTH_FIELD.to_owned(), Value::String(token.clone()));
        }
        if let Value::Object(map) = &mut request_value {
            map.insert("_stream_events".to_owned(), Value::Bool(stream_events));
        }
        let request_line = json_line(&request_value);
        stream
            .write_all(&request_line)
            .await
            .map_err(GatewayClientError::Transport)?;
        stream
            .shutdown()
            .await
            .map_err(GatewayClientError::Transport)?;
        if stream_events {
            read_response_stream(stream, on_event).await
        } else {
            read_response_line(stream).await
        }
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

async fn read_response_stream<S, F>(stream: S, mut on_event: F) -> Result<Value, GatewayClientError>
where
    S: AsyncRead + Unpin,
    F: FnMut(&Value),
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
        let value = serde_json::from_slice::<Value>(&line).map_err(GatewayClientError::Json)?;
        if value.get("event").is_some() {
            on_event(&value);
        } else {
            return Ok(value);
        }
    }
}

fn json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    line
}
