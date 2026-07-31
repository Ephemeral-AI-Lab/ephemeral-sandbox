use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::AsFd;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
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
        let host = endpoint.host.clone();
        let port = endpoint.port;
        let request_line = request_line(&request, &endpoint.auth_token)?;
        let timeout = timeout_override.unwrap_or_else(default_request_timeout);
        run_exchange(host, port, request_line, timeout)
    }
}

fn default_request_timeout() -> Duration {
    Duration::from_secs_f64(sandbox_protocol::ProtocolLimits::DEFAULT_REQUEST_READ_TIMEOUT_S)
}

fn run_exchange(
    host: String,
    port: u16,
    request_line: Vec<u8>,
    timeout: Duration,
) -> Result<OperationResponse, ManagerError> {
    let started = Instant::now();
    let addresses = resolve_addresses(&host, port, started, timeout)?;
    let mut stream = connect(&host, port, addresses, started, timeout)?;
    stream
        .set_nodelay(true)
        .map_err(|error| forwarding_error("configure daemon TCP_NODELAY", error, timeout))?;
    stream
        .set_nonblocking(true)
        .map_err(|error| forwarding_error("configure daemon nonblocking mode", error, timeout))?;
    write_request(&mut stream, &request_line, started, timeout)?;
    remaining_timeout(started, timeout)?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| forwarding_error("shutdown daemon request stream", error, timeout))?;
    read_response_line(stream, started, timeout)
}

fn resolve_addresses(
    host: &str,
    port: u16,
    started: Instant,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, ManagerError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let (sender, receiver) = mpsc::sync_channel(1);
    let host_owned = host.to_owned();
    thread::Builder::new()
        .name("sandbox-daemon-resolver".to_owned())
        .spawn(move || {
            let result = (host_owned.as_str(), port)
                .to_socket_addrs()
                .map(Iterator::collect::<Vec<_>>);
            let _ = sender.send(result);
        })
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("spawn resolver for {host}:{port} failed: {error}"),
        })?;

    receiver
        .recv_timeout(remaining_timeout(started, timeout)?)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => timeout_error(timeout),
            mpsc::RecvTimeoutError::Disconnected => ManagerError::ForwardingFailed {
                message: format!("resolver for {host}:{port} disconnected"),
            },
        })?
        .map_err(|error| ManagerError::ForwardingFailed {
            message: format!("resolve {host}:{port} failed: {error}"),
        })
}

fn connect(
    host: &str,
    port: u16,
    addresses: impl IntoIterator<Item = SocketAddr>,
    started: Instant,
    timeout: Duration,
) -> Result<TcpStream, ManagerError> {
    let mut attempted = false;
    let mut last_error = None;
    for address in addresses {
        attempted = true;
        let remaining = remaining_timeout(started, timeout)?;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) if is_timeout(&error) => return Err(timeout_error(timeout)),
            Err(error) => last_error = Some(error),
        }
    }
    let message = if attempted {
        format!(
            "connect {host}:{port} failed: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no address accepted the connection".to_owned())
        )
    } else {
        format!("connect {host}:{port} failed: address resolution returned no addresses")
    };
    Err(ManagerError::ForwardingFailed { message })
}

fn write_request(
    stream: &mut TcpStream,
    mut request_line: &[u8],
    started: Instant,
    timeout: Duration,
) -> Result<(), ManagerError> {
    while !request_line.is_empty() {
        wait_for_io(
            stream,
            PollFlags::POLLOUT,
            started,
            timeout,
            "wait to write daemon request",
        )?;
        match stream.write(request_line) {
            Ok(0) => {
                return Err(ManagerError::ForwardingFailed {
                    message: "write daemon request failed: connection accepted zero bytes"
                        .to_owned(),
                });
            }
            Ok(written) => request_line = &request_line[written..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(forwarding_error("write daemon request", error, timeout));
            }
        }
    }
    Ok(())
}

fn remaining_timeout(started: Instant, timeout: Duration) -> Result<Duration, ManagerError> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| timeout_error(timeout))
}

fn read_response_line(
    stream: TcpStream,
    started: Instant,
    timeout: Duration,
) -> Result<OperationResponse, ManagerError> {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        remaining_timeout(started, timeout)?;
        if reader.buffer().is_empty() {
            wait_for_io(
                reader.get_ref(),
                PollFlags::POLLIN,
                started,
                timeout,
                "wait for daemon response",
            )?;
        }
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => {
                return Err(forwarding_error("read daemon response", error, timeout));
            }
        };
        if available.is_empty() {
            break;
        }
        let chunk_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let remaining_capacity = MAX_RESPONSE_BYTES
            .saturating_add(1)
            .saturating_sub(line.len());
        let copied = chunk_len.min(remaining_capacity);
        line.extend_from_slice(&available[..copied]);
        reader.consume(copied);
        if line.len() > MAX_RESPONSE_BYTES {
            return Err(ManagerError::ForwardingFailed {
                message: format!("daemon response exceeded {MAX_RESPONSE_BYTES} bytes"),
            });
        }
        if copied < chunk_len || line.ends_with(b"\n") {
            break;
        }
    }
    if line.is_empty() {
        return Err(ManagerError::ForwardingFailed {
            message: "daemon returned an empty response".to_owned(),
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

fn wait_for_io(
    stream: &TcpStream,
    events: PollFlags,
    started: Instant,
    timeout: Duration,
    stage: &str,
) -> Result<(), ManagerError> {
    loop {
        let remaining_ns = remaining_timeout(started, timeout)?.as_nanos();
        let timeout_ms = remaining_ns
            .saturating_add(999_999)
            .checked_div(1_000_000)
            .unwrap_or(u128::MAX)
            .max(1);
        let poll_timeout = PollTimeout::try_from(timeout_ms).unwrap_or(PollTimeout::MAX);
        let descriptor = PollFd::new(stream.as_fd(), events);
        let mut descriptors = [descriptor];
        match poll(&mut descriptors, poll_timeout) {
            Ok(result) if result > 0 => {
                let returned = descriptors[0].revents().unwrap_or(PollFlags::POLLNVAL);
                if returned.contains(PollFlags::POLLNVAL) {
                    return Err(ManagerError::ForwardingFailed {
                        message: format!("{stage} failed: invalid daemon socket"),
                    });
                }
                return Ok(());
            }
            Ok(_) => return Err(timeout_error(timeout)),
            Err(Errno::EINTR) => {}
            Err(error) => {
                return Err(ManagerError::ForwardingFailed {
                    message: format!("{stage} failed: {error}"),
                });
            }
        }
    }
}

fn forwarding_error(stage: &str, error: std::io::Error, timeout: Duration) -> ManagerError {
    if is_timeout(&error) {
        timeout_error(timeout)
    } else {
        ManagerError::ForwardingFailed {
            message: format!("{stage} failed: {error}"),
        }
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn timeout_error(timeout: Duration) -> ManagerError {
    ManagerError::ForwardingFailed {
        message: format!("daemon request timed out after {} ms", timeout.as_millis()),
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
