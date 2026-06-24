use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::{ManagerError, SandboxDaemonEndpoint};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const MAX_RESPONSE_BYTES: usize = sandbox_protocol::MAX_REQUEST_BYTES;

pub trait SandboxDaemonClient: Send + Sync {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: sandbox_protocol::Request,
    ) -> Result<sandbox_protocol::Response, ManagerError>;

    fn invoke_with_timeout(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: sandbox_protocol::Request,
        _timeout: Duration,
    ) -> Result<sandbox_protocol::Response, ManagerError> {
        self.invoke(endpoint, request)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UnixSandboxDaemonClient;

impl UnixSandboxDaemonClient {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SandboxDaemonClient for UnixSandboxDaemonClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: sandbox_protocol::Request,
    ) -> Result<sandbox_protocol::Response, ManagerError> {
        self.invoke_with_timeout(
            endpoint,
            request,
            Duration::from_secs_f64(sandbox_protocol::REQUEST_READ_TIMEOUT_S),
        )
    }

    fn invoke_with_timeout(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: sandbox_protocol::Request,
        timeout: Duration,
    ) -> Result<sandbox_protocol::Response, ManagerError> {
        let socket_path = endpoint.socket_path.clone();
        let request_line = request_line(&request)?;
        if tokio::runtime::Handle::try_current().is_ok() {
            let worker = thread::Builder::new()
                .name("sandbox-daemon-client".to_owned())
                .spawn(move || run_exchange(socket_path, request_line, timeout))
                .map_err(|error| ManagerError::ForwardingFailed {
                    message: format!("failed to spawn daemon client worker: {error}"),
                })?;
            return worker.join().map_err(|_| ManagerError::ForwardingFailed {
                message: "daemon client worker panicked".to_owned(),
            })?;
        }
        run_exchange(socket_path, request_line, timeout)
    }
}

fn run_exchange(
    socket_path: PathBuf,
    request_line: Vec<u8>,
    timeout: Duration,
) -> Result<sandbox_protocol::Response, ManagerError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("failed to build daemon client runtime: {error}"),
        })?;
    runtime.block_on(async move {
        tokio::time::timeout(timeout, unix_exchange(socket_path, request_line))
            .await
            .map_err(|_| ManagerError::ForwardingFailed {
                message: format!("daemon request timed out after {} ms", timeout.as_millis()),
            })?
    })
}

async fn unix_exchange(
    socket_path: PathBuf,
    request_line: Vec<u8>,
) -> Result<sandbox_protocol::Response, ManagerError> {
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|error| {
        ManagerError::ForwardingFailed {
            message: format!("connect {} failed: {error}", socket_path.display()),
        }
    })?;
    stream
        .write_all(&request_line)
        .await
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("write daemon request failed: {error}"),
        })?;
    stream
        .shutdown()
        .await
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("shutdown daemon request stream failed: {error}"),
        })?;
    read_response_line(stream)
        .await
        .map(sandbox_protocol::Response::ok)
}

async fn read_response_line(stream: UnixStream) -> Result<Value, ManagerError> {
    let limit = u64::try_from(MAX_RESPONSE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reader = BufReader::new(stream.take(limit));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("read daemon response failed: {error}"),
        })?;
    if line.is_empty() {
        return Err(ManagerError::ForwardingFailed {
            message: "daemon returned an empty response".to_owned(),
        });
    }
    if line.len() > MAX_RESPONSE_BYTES {
        return Err(ManagerError::ForwardingFailed {
            message: format!("daemon response exceeded {MAX_RESPONSE_BYTES} bytes"),
        });
    }
    if !line.ends_with(b"\n") {
        return Err(ManagerError::ForwardingFailed {
            message: "daemon response was not newline terminated".to_owned(),
        });
    }
    serde_json::from_slice::<Value>(&line).map_err(|error| ManagerError::ForwardingFailed {
        message: format!("decode daemon response failed: {error}"),
    })
}

fn request_line(request: &sandbox_protocol::Request) -> Result<Vec<u8>, ManagerError> {
    let mut line = serde_json::to_vec(request).map_err(|error| ManagerError::ForwardingFailed {
        message: format!("encode daemon request failed: {error}"),
    })?;
    if line.len().saturating_add(1) > sandbox_protocol::MAX_REQUEST_BYTES {
        return Err(ManagerError::ForwardingFailed {
            message: format!(
                "daemon request exceeds {} byte limit",
                sandbox_protocol::MAX_REQUEST_BYTES
            ),
        });
    }
    line.push(b'\n');
    Ok(line)
}
