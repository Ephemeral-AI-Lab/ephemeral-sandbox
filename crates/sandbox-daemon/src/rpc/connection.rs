use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use sandbox_operation_contract::OperationResponse;
use sandbox_protocol::ProtocolLimits;

use super::{error_response, SandboxDaemonServer};
use crate::rpc::error::SandboxDaemonError;

impl SandboxDaemonServer {
    /// Handle one accepted connection: read one capped, timed request line, pop
    /// the TCP-only auth token, decode the request, dispatch, write one framed
    /// response. Per-connection; never holds a lock across the await points.
    pub(super) async fn handle_connection<S>(
        &self,
        stream: S,
        is_tcp: bool,
        _peer_addr: Option<SocketAddr>,
        _local_addr: Option<SocketAddr>,
    ) -> Result<(), SandboxDaemonError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let bytes = read_request_line(&mut reader, self.config.limits).await;
        let response = match bytes {
            Ok(bytes) => self.dispatch_bytes(bytes, is_tcp).await,
            Err(err) => self.read_error_response(err, is_tcp),
        };
        let framed = encode_response(&response);
        write_response_with_limits(&mut writer, &framed, self.config.limits).await
    }

    fn read_error_response(&self, err: SandboxDaemonError, _is_tcp: bool) -> OperationResponse {
        match err {
            err @ SandboxDaemonError::RequestTooLarge { limit } => error_response(
                err.response_kind(),
                format!("daemon request exceeds {limit} byte limit"),
                serde_json::json!({"limit": limit}),
            ),
            err => error_response(err.response_kind(), err.to_string(), serde_json::json!({})),
        }
    }
}

fn encode_response(response: &OperationResponse) -> Vec<u8> {
    sandbox_protocol::response_line(response)
}

async fn read_request_line<R>(
    reader: &mut R,
    limits: ProtocolLimits,
) -> Result<Vec<u8>, SandboxDaemonError>
where
    R: AsyncRead + Unpin,
{
    read_request_line_with_limits(reader, limits).await
}

/// Write and close a response within a deadline independent of operation work.
/// A caller that stops receiving cannot consume a long-running operation's
/// entire forwarding budget after the durable outcome has already returned.
pub(crate) async fn write_response_with_limits<W>(
    writer: &mut W,
    framed: &[u8],
    limits: ProtocolLimits,
) -> Result<(), SandboxDaemonError>
where
    W: AsyncWrite + Unpin,
{
    let write = async {
        writer.write_all(framed).await?;
        writer.shutdown().await
    };
    timeout(
        Duration::from_secs_f64(limits.response_write_timeout_s),
        write,
    )
    .await
    .map_err(|_| {
        SandboxDaemonError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "daemon response write timed out",
        ))
    })??;
    Ok(())
}

pub(crate) async fn read_request_line_with_limits<R>(
    reader: &mut R,
    limits: ProtocolLimits,
) -> Result<Vec<u8>, SandboxDaemonError>
where
    R: AsyncRead + Unpin,
{
    let max_request_bytes = limits.max_request_bytes;
    let mut buf = Vec::new();
    let read = async {
        let limit = u64::try_from(max_request_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut limited = BufReader::new(reader.take(limit));
        limited.read_until(b'\n', &mut buf).await?;
        if buf.len() > max_request_bytes {
            return Err(SandboxDaemonError::RequestTooLarge {
                limit: max_request_bytes,
            });
        }
        Ok::<(), SandboxDaemonError>(())
    };
    timeout(Duration::from_secs_f64(limits.request_read_timeout_s), read)
        .await
        .map_err(|_| {
            SandboxDaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon request read timed out",
            ))
        })??;
    Ok(buf)
}
