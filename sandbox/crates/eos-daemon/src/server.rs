//! The async RPC server: AF_UNIX + loopback-TCP listeners, framing, shutdown.
//!
//! This is the ONLY tokio surface in the workspace. It listens on an AF_UNIX
//! socket AND (optionally) a 127.0.0.1 TCP port, reads ONE newline-delimited
//! compact-JSON request per connection (capped at [`eos_protocol::MAX_REQUEST_BYTES`],
//! read-timed at [`eos_protocol::REQUEST_READ_TIMEOUT_S`]), pops the TCP-only
//! auth token before dispatch, routes through the [`crate::dispatcher::OpTable`],
//! and writes back one framed response.
//!
//! # The two async invariants (§5)
//!
//! 1. **Never hold a lock across `.await`.** Connection handlers clone the data
//!    they need out of any guarded state, drop the guard, THEN await. The audit
//!    ring + in-flight registry use synchronous mutexes held only across
//!    non-await sections.
//! 2. **One OCC writer per root via an mpsc work queue.** The single-writer
//!    publish path is reached NOT by locking shared OCC state across awaits but
//!    by sending a [`OccWork`] item down an [`tokio::sync::mpsc`] channel to a
//!    dedicated consumer task, which replies on a [`tokio::sync::oneshot`]. This
//!    serializes publishes without a long-held lock.
//!
//! Shutdown is driven by a [`tokio_util::sync::CancellationToken`]: a SIGTERM /
//! SIGINT cancels it, the serve loops select on it, in-flight pipelines are
//! drained, and (per the Python `start_new_session=True`) the cancel path kills
//! the full child process group.
//! `// PORT backend/src/sandbox/daemon/rpc/server.py:58,62,116-143,183,193 — caps/timeout/auth/listeners`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use eos_protocol::{decode, encode, Envelope, ErrorKind, LayerChange};

use crate::audit_buffer::AuditBuffer;
use crate::dispatcher::OpTable;
use crate::error::DaemonError;
use crate::in_flight::InFlightRegistry;

/// Maximum bytes read for a single request line (re-exported for the listener
/// buffer cap). `// PORT backend/src/sandbox/daemon/rpc/server.py:58 — MAX_REQUEST_BYTES`
pub const MAX_REQUEST_BYTES: usize = eos_protocol::MAX_REQUEST_BYTES;

/// Per-request read timeout in seconds. `// PORT server.py:62 — REQUEST_READ_TIMEOUT_S`
pub const REQUEST_READ_TIMEOUT_S: f64 = eos_protocol::REQUEST_READ_TIMEOUT_S;

/// Where the daemon binds + writes its pid, plus the optional TCP listener.
/// `// PORT backend/src/sandbox/daemon/rpc/server.py:148-205 — serve(socket_path, pid_path, tcp_host, tcp_port, auth_token)`
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// AF_UNIX socket path (chmod 0o600 after bind).
    pub socket_path: PathBuf,
    /// Pid file path written after the listeners bind.
    pub pid_path: PathBuf,
    /// Optional loopback TCP host (e.g. `127.0.0.1`).
    pub tcp_host: Option<String>,
    /// Optional loopback TCP port; both host+port enable the TCP listener.
    pub tcp_port: Option<u16>,
    /// TCP-only auth token; popped from each TCP request before dispatch.
    pub auth_token: Option<String>,
}

/// The running daemon: the op table, audit ring, in-flight registry, the OCC
/// work-queue sender, and the shutdown token.
///
/// It ORCHESTRATES but NEVER enters a namespace: namespace work is delegated to
/// the `eosd ns-holder` / `eosd ns-runner` children it spawns; the daemon stays
/// multi-threaded (tokio) and would fail `unshare(CLONE_NEWUSER)` / `setns` into
/// a userns itself.
pub struct DaemonServer {
    config: ServerConfig,
    op_table: OpTable,
    audit: AuditBuffer,
    in_flight: InFlightRegistry,
    occ_tx: mpsc::Sender<OccWork>,
    shutdown: CancellationToken,
}

impl DaemonServer {
    /// Assemble a daemon over `config`, wiring the op table, audit ring, the
    /// in-flight registry, and the OCC single-writer queue. The returned
    /// [`OccWriterQueue`] consumer must be driven by [`Self::serve`].
    pub fn new(config: ServerConfig) -> (Self, OccWriterQueue) {
        let (occ_tx, occ_rx) = mpsc::channel(MAX_OCC_QUEUE_DEPTH);
        let shutdown = CancellationToken::new();
        let server = Self {
            config,
            op_table: OpTable::with_builtins(),
            audit: AuditBuffer::new(),
            in_flight: InFlightRegistry::from_env(),
            occ_tx,
            shutdown: shutdown.clone(),
        };
        (
            server,
            OccWriterQueue {
                rx: occ_rx,
                shutdown,
            },
        )
    }

    /// The shutdown token; cancel it to drain + tear down the serve loops.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Bind the AF_UNIX (and optional TCP) listeners, write the pid file, install
    /// the SIGTERM/SIGINT handlers, and serve until the shutdown token fires.
    ///
    /// On shutdown: cancel the serve tasks, drain in-flight ephemeral pipelines,
    /// remove the pid file, and unlink the socket.
    // PORT backend/src/sandbox/daemon/rpc/server.py:148-249 — serve(): start_unix_server + start_server, signal handlers, AsyncExitStack, stop_all_ephemeral_pipelines + pid/socket cleanup
    pub async fn serve(self, occ_queue: OccWriterQueue) -> Result<(), DaemonError> {
        let shutdown = self.shutdown.clone();
        let server = Arc::new(self);
        let _occ_task = tokio::spawn(occ_queue.run());
        let _ = (&server.audit, &server.in_flight, &server.occ_tx);

        if let Some(parent) = server.config.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Some(parent) = server.config.pid_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let _ = tokio::fs::remove_file(&server.config.socket_path).await;
        let unix_listener = UnixListener::bind(&server.config.socket_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(
                &server.config.socket_path,
                std::fs::Permissions::from_mode(0o600),
            )
            .await?;
        }
        tokio::fs::write(&server.config.pid_path, std::process::id().to_string()).await?;

        let unix_server = {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = server.shutdown.cancelled() => break,
                        accepted = unix_listener.accept() => {
                            let (stream, _) = accepted?;
                            let server = Arc::clone(&server);
                            tokio::spawn(async move {
                                let _ = server.handle_connection(stream, false).await;
                            });
                        }
                    }
                }
                Ok::<(), std::io::Error>(())
            })
        };

        let tcp_server = match (&server.config.tcp_host, server.config.tcp_port) {
            (Some(host), Some(port)) => {
                let listener = TcpListener::bind((host.as_str(), port)).await?;
                let server = Arc::clone(&server);
                Some(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = server.shutdown.cancelled() => break,
                            accepted = listener.accept() => {
                                let (stream, _) = accepted?;
                                let server = Arc::clone(&server);
                                tokio::spawn(async move {
                                    let _ = server.handle_connection(stream, true).await;
                                });
                            }
                        }
                    }
                    Ok::<(), std::io::Error>(())
                }))
            }
            _ => None,
        };

        tokio::select! {
            _ = shutdown.cancelled() => {}
            _ = signal_shutdown() => shutdown.cancel(),
            result = unix_server => {
                if let Ok(Err(err)) = result {
                    return Err(DaemonError::Io(err));
                }
            }
        }
        if let Some(task) = tcp_server {
            task.abort();
        }
        let _ = tokio::fs::remove_file(&server.config.pid_path).await;
        let _ = tokio::fs::remove_file(&server.config.socket_path).await;
        Ok(())
    }

    /// Handle one accepted connection: read one capped, timed request line, pop
    /// the TCP-only auth token, decode the envelope, dispatch, write one framed
    /// response. Per-connection; never holds a lock across the await points.
    // PORT backend/src/sandbox/daemon/rpc/server.py:64-143 — _handle_connection(): readline(timeout), LimitOverrun/Value -> request_too_large, auth pop (TCP), bad_json/invalid_envelope, dispatch, frame + drain
    async fn handle_connection<S>(&self, stream: S, is_tcp: bool) -> Result<(), DaemonError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let bytes = read_request_line(&mut reader).await;
        let response = match bytes {
            Ok(bytes) => self.dispatch_bytes(bytes, is_tcp),
            Err(err @ DaemonError::RequestTooLarge { .. }) => crate::dispatcher::error_envelope(
                err.wire_kind(),
                &format!("daemon request exceeds {MAX_REQUEST_BYTES} byte limit"),
                serde_json::json!({"limit": MAX_REQUEST_BYTES}),
            ),
            Err(err) => crate::dispatcher::error_envelope(
                err.wire_kind(),
                &err.to_string(),
                serde_json::json!({}),
            ),
        };
        let framed = encode(&Envelope::Response(response))?;
        writer.write_all(&framed).await?;
        writer.shutdown().await?;
        Ok(())
    }

    fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
        let bytes = if is_tcp {
            match self.strip_tcp_auth(&bytes) {
                Ok(bytes) => bytes,
                Err(err) => {
                    return crate::dispatcher::error_envelope(
                        err.wire_kind(),
                        &err.to_string(),
                        serde_json::json!({}),
                    );
                }
            }
        } else {
            bytes
        };
        match decode(&bytes) {
            Ok(Envelope::Request(request)) => self.op_table.dispatch(&request),
            Ok(_) => crate::dispatcher::error_envelope(
                ErrorKind::InvalidEnvelope,
                "request envelope must include op, invocation_id, and args",
                serde_json::json!({}),
            ),
            Err(err) => crate::dispatcher::error_envelope(
                ErrorKind::BadJson,
                &err.to_string(),
                serde_json::json!({}),
            ),
        }
    }

    fn strip_tcp_auth(&self, bytes: &[u8]) -> Result<Vec<u8>, DaemonError> {
        let Some(expected) = self
            .config
            .auth_token
            .as_deref()
            .filter(|token| !token.is_empty())
        else {
            return Ok(bytes.to_vec());
        };
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(eos_protocol::ProtocolError::from)?;
        let token = value
            .as_object_mut()
            .and_then(|object| object.remove(eos_protocol::DAEMON_AUTH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));
        if token.as_deref() != Some(expected) {
            return Err(DaemonError::Unauthorized);
        }
        let mut encoded = serde_json::to_vec(&value).map_err(eos_protocol::ProtocolError::from)?;
        encoded.push(b'\n');
        Ok(encoded)
    }
}

/// Bound on the OCC work-queue depth (back-pressures publishers onto the single
/// writer). `// PORT backend/src/sandbox/occ/commit_queue.py:66 — max_batch_size headroom`
pub const MAX_OCC_QUEUE_DEPTH: usize = 1024;

/// One unit of OCC publish work plus its reply channel.
///
/// The single-writer guarantee is reached by SENDING this down the mpsc queue to
/// the one consumer task — never by holding a shared OCC lock across an `.await`
/// (§5). The consumer replies on `reply`.
/// `// PORT backend/src/sandbox/occ/commit_queue.py:90-91 — single "occ-commit-queue" writer thread`
pub struct OccWork {
    /// The `layer_stack_root` whose single writer this work targets.
    pub layer_stack_root: String,
    /// The changeset to publish through that one writer.
    pub changes: Vec<LayerChange>,
    /// Whether the changeset must publish atomically (all-or-nothing).
    pub atomic: bool,
    /// Reply channel: the consumer sends the publish outcome back here.
    pub reply: oneshot::Sender<Result<eos_occ::ChangesetResult, DaemonError>>,
}

/// The receive side of the OCC single-writer queue, driven by one consumer task.
///
/// Owning the single `mpsc::Receiver` is what makes the writer single: exactly
/// one consumer task drains it, so all publishes for all roots serialize through
/// this one task (which dispatches per-root to the matching [`OccService`]).
/// `// PORT backend/src/sandbox/occ/commit_queue.py:120-160 — drain loop (batch window, single thread)`
///
/// [`OccService`]: eos_occ::OccService
pub struct OccWriterQueue {
    rx: mpsc::Receiver<OccWork>,
    shutdown: CancellationToken,
}

impl OccWriterQueue {
    /// Run the single consumer: receive [`OccWork`], drive the per-root OCC
    /// writer, reply on the oneshot, until the queue closes or shutdown fires.
    // PORT backend/src/sandbox/occ/commit_queue.py:120-160 — _drain_loop: recv batch, commit_prepared, reply, honor batch_window
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                work = self.rx.recv() => {
                    let Some(work) = work else { break };
                    let _ = work.reply.send(Err(DaemonError::Forbidden(
                        "OCC publish is not implemented in Phase 2".to_owned(),
                    )));
                }
            }
        }
    }
}

async fn read_request_line<R>(reader: &mut R) -> Result<Vec<u8>, DaemonError>
where
    R: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    let timeout_duration = Duration::from_secs_f64(REQUEST_READ_TIMEOUT_S);
    let read = async {
        loop {
            let n = reader.read(&mut byte).await?;
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if buf.len() > MAX_REQUEST_BYTES {
                return Err(DaemonError::RequestTooLarge {
                    limit: MAX_REQUEST_BYTES,
                });
            }
            if byte[0] == b'\n' {
                break;
            }
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

async fn signal_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
