use std::time::Duration;

use sandbox_protocol::{decode_request_object, ArgsPresence, SandboxRequest};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use super::{SandboxManagerServer, ServerError};

impl SandboxManagerServer {
    pub async fn handle_connection<S>(&self, stream: S) -> Result<(), ServerError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let response = match read_request_line(&mut reader).await {
            Ok(bytes) => match decode_request_bytes(&bytes) {
                Ok(request) => self.dispatch_request(request).await,
                Err(error) => error.to_response_value(),
            },
            Err(error) => error.to_response_value(),
        };
        writer
            .write_all(&sandbox_protocol::response_line(&response))
            .await?;
        writer.shutdown().await?;
        Ok(())
    }
}

async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, ServerError>
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
            return Err(ServerError::RequestTooLarge {
                limit: sandbox_protocol::MAX_REQUEST_BYTES,
            });
        }
        Ok::<(), ServerError>(())
    };
    timeout(
        Duration::from_secs_f64(sandbox_protocol::REQUEST_READ_TIMEOUT_S),
        read,
    )
    .await
    .map_err(|_| {
        ServerError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "manager request read timed out",
        ))
    })??;
    Ok(buf)
}

fn decode_request_bytes(bytes: &[u8]) -> Result<SandboxRequest, ServerError> {
    let value = serde_json::from_slice::<Value>(bytes)?;
    let Value::Object(object) = value else {
        return Err(ServerError::BadRequest {
            kind: sandbox_protocol::error_kind::BAD_JSON,
            message: "request message must be a json object".to_owned(),
        });
    };
    decode_request_object(object, ArgsPresence::Required).map_err(|error| ServerError::BadRequest {
        kind: error.kind(),
        message: error.message().to_owned(),
    })
}
