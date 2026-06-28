use std::time::Duration;

use sandbox_manager::ManagerProgressEvent;
use sandbox_protocol::{decode_request_value, Request};
use serde_json::{json, Value};
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
                Ok((request, stream_events)) if stream_events => {
                    return self.handle_streaming_request(request, &mut writer).await;
                }
                Ok((request, _)) => self
                    .manager
                    .dispatch_request(request)
                    .await
                    .into_json_value(),
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

    async fn handle_streaming_request<W>(
        &self,
        request: Request,
        writer: &mut W,
    ) -> Result<(), GatewayError>
    where
        W: AsyncWrite + Unpin,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let progress = sandbox_manager::ProgressSink::new(move |event| {
            let _ = tx.send(progress_event_value(event));
        });
        let manager = self.manager.clone();
        let response_task = tokio::spawn(async move {
            manager
                .dispatch_request_with_progress(request, progress)
                .await
                .into_json_value()
        });
        while let Some(event) = rx.recv().await {
            writer
                .write_all(&sandbox_protocol::response_line(&event))
                .await?;
        }
        let response = response_task.await.map_err(|error| {
            GatewayError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("gateway streaming task failed: {error}"),
            ))
        })?;
        writer
            .write_all(&sandbox_protocol::response_line(&response))
            .await?;
        writer.shutdown().await?;
        Ok(())
    }

    fn authorize_and_decode(&self, bytes: &[u8]) -> Result<(Request, bool), GatewayError> {
        let value = serde_json::from_slice::<Value>(bytes)?;
        let Value::Object(mut object) = value else {
            return decode_request(value).map(|request| (request, false));
        };
        let stream_events = object
            .remove("_stream_events")
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
        decode_request(Value::Object(object)).map(|request| (request, stream_events))
    }
}

fn progress_event_value(event: ManagerProgressEvent) -> Value {
    json!({
        "event": "progress",
        "progress": {
            "op": event.op,
            "phase": event.phase,
            "state": event.state,
            "message": event.message,
            "sandbox_id": event.sandbox_id,
            "elapsed_ms": event.elapsed_ms,
        },
    })
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
