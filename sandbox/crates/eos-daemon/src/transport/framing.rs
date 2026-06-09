//! Request framing: the per-request byte cap, read timeout, the single capped +
//! timed line reader, and the shutdown-signal future.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::time::timeout;

use crate::error::DaemonError;

/// Maximum bytes read for a single request line.
pub(super) const MAX_REQUEST_BYTES: usize = eos_protocol::MAX_REQUEST_BYTES;

/// Per-request read timeout in seconds.
const REQUEST_READ_TIMEOUT_S: f64 = eos_protocol::REQUEST_READ_TIMEOUT_S;

pub(super) async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, DaemonError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let timeout_duration = Duration::from_secs_f64(REQUEST_READ_TIMEOUT_S);
    let read = async {
        // Bound the buffered read to one byte past the cap so a frame without a
        // newline cannot grow `buf` without limit (a no-newline flood OOM); the
        // explicit length check below preserves the existing `RequestTooLarge`.
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
    timeout(timeout_duration, read).await.map_err(|_| {
        DaemonError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "daemon request read timed out",
        ))
    })??;
    Ok(buf)
}

pub(super) async fn signal_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
