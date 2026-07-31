use std::time::Duration;

use sandbox_operation_contract::OperationRequest;
use sandbox_protocol::decode_request_value;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::{GatewayError, SandboxGatewayServer};

impl SandboxGatewayServer {
    pub async fn handle_connection<S>(&self, stream: S) -> Result<(), GatewayError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let bytes = read_request_line(&mut reader).await;
        let response = match bytes {
            Ok(bytes) => match self.authorize_and_decode(&bytes) {
                Ok((request, stream_logs)) if stream_logs => {
                    return self.handle_streaming_request(request, &mut writer).await;
                }
                Ok((request, _)) => self.manager.dispatch_request(request).await,
                Err(error) => error.to_response(),
            },
            Err(error) => error.to_response(),
        };
        let framed = sandbox_protocol::response_line(&response);
        write_framed_response_with_timeout(&mut writer, &framed).await
    }

    async fn handle_streaming_request<W>(
        &self,
        request: OperationRequest,
        writer: &mut W,
    ) -> Result<(), GatewayError>
    where
        W: AsyncWrite + Unpin,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let progress = sandbox_manager::ProgressSink::new(move |log| {
            let _ = tx.send(log);
        });
        let manager = self.manager.clone();
        let response_task = tokio::spawn(async move {
            manager
                .dispatch_request_with_progress(request, progress)
                .await
        });
        while let Some(log) = rx.recv().await {
            let framed = cli_log_line(&log);
            write_progress_with_timeout(writer, &framed).await?;
        }
        let response = response_task.await.map_err(|error| {
            GatewayError::Io(std::io::Error::other(format!(
                "gateway streaming task failed: {error}"
            )))
        })?;
        let framed = sandbox_protocol::response_line(&response);
        write_framed_response_with_timeout(writer, &framed).await
    }

    fn authorize_and_decode(&self, bytes: &[u8]) -> Result<(OperationRequest, bool), GatewayError> {
        let value = serde_json::from_slice::<Value>(bytes)?;
        let Value::Object(mut object) = value else {
            return decode_request(value).map(|request| (request, false));
        };
        let stream_logs = object
            .remove("_stream_logs")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let presented = object
            .remove(sandbox_protocol::GATEWAY_AUTH_FIELD)
            .and_then(|token| token.as_str().map(str::to_owned));
        if let Some(expected) = self.config.auth_token.as_deref() {
            if presented.as_deref() != Some(expected) {
                return Err(GatewayError::Unauthorized);
            }
        }
        decode_request(Value::Object(object)).map(|request| (request, stream_logs))
    }
}

pub(crate) async fn write_framed_response_with_timeout<W>(
    writer: &mut W,
    framed: &[u8],
) -> Result<(), GatewayError>
where
    W: AsyncWrite + Unpin,
{
    let write = async {
        writer.write_all(framed).await?;
        writer.shutdown().await
    };
    timeout(
        Duration::from_secs_f64(sandbox_protocol::ProtocolLimits::DEFAULT_RESPONSE_WRITE_TIMEOUT_S),
        write,
    )
    .await
    .map_err(|_| {
        GatewayError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "gateway response write timed out",
        ))
    })??;
    Ok(())
}

async fn write_progress_with_timeout<W>(writer: &mut W, framed: &[u8]) -> Result<(), GatewayError>
where
    W: AsyncWrite + Unpin,
{
    timeout(
        Duration::from_secs_f64(sandbox_protocol::ProtocolLimits::DEFAULT_RESPONSE_WRITE_TIMEOUT_S),
        writer.write_all(framed),
    )
    .await
    .map_err(|_| {
        GatewayError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "gateway progress write timed out",
        ))
    })??;
    Ok(())
}

fn cli_log_line(message: &str) -> Vec<u8> {
    let escaped = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".to_owned());
    format!("cli_log({escaped})\n").into_bytes()
}

async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, GatewayError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let read = async {
        let max_request_bytes = sandbox_protocol::ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES;
        let limit = u64::try_from(max_request_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut limited = BufReader::new(reader.take(limit));
        limited.read_until(b'\n', &mut buf).await?;
        if buf.len() > max_request_bytes {
            return Err(GatewayError::RequestTooLarge {
                limit: max_request_bytes,
            });
        }
        if !buf.ends_with(b"\n") {
            return Err(GatewayError::MissingNewline);
        }
        Ok::<(), GatewayError>(())
    };
    timeout(
        Duration::from_secs_f64(sandbox_protocol::ProtocolLimits::DEFAULT_REQUEST_READ_TIMEOUT_S),
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

fn decode_request(value: Value) -> Result<OperationRequest, GatewayError> {
    decode_request_value(value).map_err(|error| GatewayError::BadRequest {
        kind: error.kind(),
        message: error.message().to_owned(),
    })
}
