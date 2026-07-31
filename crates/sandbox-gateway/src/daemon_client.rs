use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use sandbox_manager::{ManagerError, SandboxDaemonClient, SandboxDaemonEndpoint};
use sandbox_operation_contract::{OperationRequest, OperationResponse};

const MAX_RESPONSE_BYTES: usize = sandbox_protocol::ProtocolLimits::DEFAULT_MAX_REQUEST_BYTES;

#[derive(Debug, Default, Clone, Copy)]
pub struct TcpSandboxDaemonClient;

impl TcpSandboxDaemonClient {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SandboxDaemonClient for TcpSandboxDaemonClient {
    fn invoke(
        &self,
        endpoint: &SandboxDaemonEndpoint,
        request: OperationRequest,
        timeout_override: Option<Duration>,
    ) -> Result<OperationResponse, ManagerError> {
        let request_line = request_line(&request, &endpoint.auth_token)?;
        let timeout = timeout_override.unwrap_or_else(default_request_timeout);
        run_exchange(&endpoint.host, endpoint.port, &request_line, timeout)
    }
}

fn default_request_timeout() -> Duration {
    Duration::from_secs_f64(sandbox_protocol::ProtocolLimits::DEFAULT_REQUEST_READ_TIMEOUT_S)
}

fn run_exchange(
    host: &str,
    port: u16,
    request_line: &[u8],
    timeout: Duration,
) -> Result<OperationResponse, ManagerError> {
    let deadline = RequestDeadline::new(timeout);
    tcp_exchange(host, port, request_line, &deadline)
}

fn tcp_exchange(
    host: &str,
    port: u16,
    request_line: &[u8],
    deadline: &RequestDeadline,
) -> Result<OperationResponse, ManagerError> {
    deadline.remaining()?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| deadline.io_error(error, format!("connect {host}:{port} failed")))?;
    deadline.remaining()?;
    let mut last_connect_error = None;
    let mut stream = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, deadline.remaining()?) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => {
                if deadline.is_timeout(&error) {
                    return Err(deadline.timeout_error());
                }
                last_connect_error = Some(error);
            }
        }
    }
    let mut stream = stream.ok_or_else(|| {
        let error = last_connect_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "resolved address list was empty",
            )
        });
        deadline.io_error(error, format!("connect {host}:{port} failed"))
    })?;
    write_request(&mut stream, request_line, deadline)?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| deadline.io_error(error, "shutdown daemon request stream failed"))?;
    deadline.remaining()?;
    read_response_line(stream, deadline)
}

fn write_request(
    stream: &mut TcpStream,
    mut request_line: &[u8],
    deadline: &RequestDeadline,
) -> Result<(), ManagerError> {
    while !request_line.is_empty() {
        stream
            .set_write_timeout(Some(deadline.remaining()?))
            .map_err(|error| deadline.io_error(error, "write daemon request failed"))?;
        match stream.write(request_line) {
            Ok(0) => {
                return Err(deadline.io_error(
                    io::Error::new(io::ErrorKind::WriteZero, "failed to write whole buffer"),
                    "write daemon request failed",
                ));
            }
            Ok(written) => request_line = &request_line[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(deadline.io_error(error, "write daemon request failed"));
            }
        }
    }
    deadline.remaining()?;
    Ok(())
}

fn read_response_line(
    mut stream: TcpStream,
    deadline: &RequestDeadline,
) -> Result<OperationResponse, ManagerError> {
    let mut line = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let remaining_capacity = MAX_RESPONSE_BYTES.saturating_add(1) - line.len();
        let read_length = buffer.len().min(remaining_capacity);
        stream
            .set_read_timeout(Some(deadline.remaining()?))
            .map_err(|error| deadline.io_error(error, "read daemon response failed"))?;
        let read = match stream.read(&mut buffer[..read_length]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(deadline.io_error(error, "read daemon response failed"));
            }
        };
        if let Some(newline) = buffer[..read].iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&buffer[..=newline]);
            break;
        }
        line.extend_from_slice(&buffer[..read]);
        if line.len() > MAX_RESPONSE_BYTES {
            break;
        }
    }
    deadline.remaining()?;
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
    sandbox_protocol::decode_response_line(&line).map_err(|error| ManagerError::ForwardingFailed {
        message: format!("decode daemon response failed: {error}"),
    })
}

struct RequestDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl RequestDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    fn remaining(&self) -> Result<Duration, ManagerError> {
        let remaining = self.timeout.saturating_sub(self.started_at.elapsed());
        if remaining.is_zero() {
            return Err(self.timeout_error());
        }
        Ok(remaining)
    }

    fn is_timeout(&self, error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) || self.started_at.elapsed() >= self.timeout
    }

    fn io_error(&self, error: io::Error, context: impl std::fmt::Display) -> ManagerError {
        if self.is_timeout(&error) {
            self.timeout_error()
        } else {
            ManagerError::ForwardingFailed {
                message: format!("{context}: {error}"),
            }
        }
    }

    fn timeout_error(&self) -> ManagerError {
        ManagerError::ForwardingFailed {
            message: format!(
                "daemon request timed out after {} ms",
                self.timeout.as_millis()
            ),
        }
    }
}

fn request_line(request: &OperationRequest, auth_token: &str) -> Result<Vec<u8>, ManagerError> {
    let line = sandbox_protocol::encode_authenticated_request_line(
        request,
        sandbox_protocol::DAEMON_AUTH_FIELD,
        auth_token,
    )
    .map_err(|error| ManagerError::ForwardingFailed {
        message: format!("encode daemon request failed: {error}"),
    })?;
    if line.len() > MAX_RESPONSE_BYTES {
        return Err(ManagerError::ForwardingFailed {
            message: format!("daemon request exceeds {MAX_RESPONSE_BYTES} byte limit"),
        });
    }
    Ok(line)
}
