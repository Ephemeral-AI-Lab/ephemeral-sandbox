use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use super::trace_context::{elapsed_us, trace_facts, unix_ms, TransportTraceContext};
use super::{DaemonServer, MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S};
use crate::error::DaemonError;
use crate::wire::{encode, WireMessage};

impl DaemonServer {
    /// Handle one accepted connection: read one capped, timed request line, pop
    /// the TCP-only auth token, decode the request, dispatch, write one framed
    /// response. Per-connection; never holds a lock across the await points.
    pub(super) async fn handle_connection<S>(
        &self,
        stream: S,
        is_tcp: bool,
        peer_addr: Option<SocketAddr>,
        local_addr: Option<SocketAddr>,
    ) -> Result<(), DaemonError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let connection_id = crate::trace::next_connection_id();
        let accepted_at_unix_ms = unix_ms();
        let read_start = Instant::now();
        let bytes = read_request_line(&mut reader).await;
        let read_duration_us = elapsed_us(read_start);
        let transport_context = TransportTraceContext {
            connection_id,
            is_tcp,
            read_duration_us,
            accepted_at_unix_ms,
            peer_addr,
            local_addr,
        };
        let response = match bytes {
            Ok(bytes) => self.dispatch_bytes(bytes, transport_context).await,
            Err(err @ DaemonError::RequestTooLarge { .. }) => {
                let facts = trace_facts(
                    &transport_context,
                    MAX_REQUEST_BYTES.saturating_add(1),
                    self.tcp_auth_required(transport_context.is_tcp),
                    false,
                    None,
                );
                crate::trace::attach_request_sidecar(
                    crate::dispatcher::error_response(
                        err.wire_kind(),
                        format!("daemon request exceeds {MAX_REQUEST_BYTES} byte limit"),
                        serde_json::json!({"limit": MAX_REQUEST_BYTES}),
                    ),
                    None,
                    "daemon.transport.read",
                    &facts,
                )
            }
            Err(err) => {
                let facts = trace_facts(
                    &transport_context,
                    0,
                    self.tcp_auth_required(transport_context.is_tcp),
                    false,
                    None,
                );
                crate::trace::attach_request_sidecar(
                    crate::dispatcher::error_response(
                        err.wire_kind(),
                        err.to_string(),
                        serde_json::json!({}),
                    ),
                    None,
                    "daemon.transport.read",
                    &facts,
                )
            }
        };
        let framed = encode(&WireMessage::Response(response.clone()))?;
        if let Err(err) = writer.write_all(&framed).await {
            crate::trace::push_transport_failure_from_sidecar(
                &response,
                "response_write_failed",
                &err,
            );
            return Err(DaemonError::Io(err));
        }
        if let Err(err) = writer.shutdown().await {
            crate::trace::push_transport_failure_from_sidecar(
                &response,
                "response_shutdown_failed",
                &err,
            );
            return Err(DaemonError::Io(err));
        }
        Ok(())
    }
}

async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, DaemonError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let read = async {
        let limit = u64::try_from(MAX_REQUEST_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut limited = BufReader::new(reader.take(limit));
        limited.read_until(b'\n', &mut buf).await?;
        if buf.len() > MAX_REQUEST_BYTES {
            return Err(DaemonError::RequestTooLarge {
                limit: MAX_REQUEST_BYTES,
            });
        }
        Ok::<(), DaemonError>(())
    };
    timeout(Duration::from_secs_f64(REQUEST_READ_TIMEOUT_S), read)
        .await
        .map_err(|_| {
            DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon request read timed out",
            ))
        })??;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt as _;

    use super::*;

    #[tokio::test]
    async fn read_request_line_rejects_oversized_payloads() {
        let mut reader = tokio::io::repeat(b'x').take(
            u64::try_from(MAX_REQUEST_BYTES)
                .expect("max request bytes fits u64")
                .saturating_add(1),
        );
        let err = read_request_line(&mut reader)
            .await
            .expect_err("oversized request rejected");
        assert!(matches!(err, DaemonError::RequestTooLarge { .. }));
    }

    #[tokio::test]
    async fn read_request_line_times_out_waiting_for_line() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        let err = read_request_line(&mut reader)
            .await
            .expect_err("hanging request times out");
        assert!(
            matches!(err, DaemonError::Io(ref source) if source.kind() == std::io::ErrorKind::TimedOut),
            "{err:?}"
        );
    }
}
