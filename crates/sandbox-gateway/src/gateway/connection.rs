use std::time::Duration;

use sandbox_protocol::{decode_request_value, Request};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use super::{GatewayError, SandboxGatewayServer};
use crate::cli::timing;

impl SandboxGatewayServer {
    pub async fn handle_connection<S>(&self, stream: S) -> Result<(), GatewayError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let read_started = std::time::Instant::now();
        let bytes = read_request_line(&mut reader).await;
        timing::duration("gateway.read_request", read_started);
        let dispatch_started = std::time::Instant::now();
        let response = match bytes {
            Ok(bytes) => match self.authorize_and_decode(&bytes) {
                Ok(request) => self
                    .manager
                    .dispatch_request(request)
                    .await
                    .into_json_value(),
                Err(error) => error.to_response_value(),
            },
            Err(error) => error.to_response_value(),
        };
        timing::duration("gateway.dispatch", dispatch_started);
        let write_started = std::time::Instant::now();
        writer
            .write_all(&sandbox_protocol::response_line(&response))
            .await?;
        writer.shutdown().await?;
        timing::duration("gateway.write_response", write_started);
        Ok(())
    }

    fn authorize_and_decode(&self, bytes: &[u8]) -> Result<Request, GatewayError> {
        let value = serde_json::from_slice::<Value>(bytes)?;
        let Value::Object(mut object) = value else {
            return decode_request(value);
        };
        let presented = object
            .remove(sandbox_protocol::GATEWAY_AUTH_FIELD)
            .and_then(|token| token.as_str().map(str::to_owned));
        if let Some(expected) = self.config.auth_token.as_deref() {
            if presented.as_deref() != Some(expected) {
                return Err(GatewayError::Unauthorized);
            }
        }
        decode_request(Value::Object(object))
    }
}

async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, GatewayError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let read = async {
        let limit = u64::try_from(sandbox_protocol::MAX_REQUEST_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut limited = BufReader::new(reader.take(limit));
        limited.read_until(b'\n', &mut buf).await?;
        if buf.len() > sandbox_protocol::MAX_REQUEST_BYTES {
            return Err(GatewayError::RequestTooLarge {
                limit: sandbox_protocol::MAX_REQUEST_BYTES,
            });
        }
        if !buf.ends_with(b"\n") {
            return Err(GatewayError::MissingNewline);
        }
        Ok::<(), GatewayError>(())
    };
    timeout(
        Duration::from_secs_f64(sandbox_protocol::REQUEST_READ_TIMEOUT_S),
        read,
    )
    .await
    .map_err(|_| {
        GatewayError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "gateway request read timed out",
        ))
    })??;
    Ok(buf)
}

fn decode_request(value: Value) -> Result<Request, GatewayError> {
    decode_request_value(value).map_err(|error| GatewayError::BadRequest {
        kind: error.kind(),
        message: error.message().to_owned(),
    })
}
