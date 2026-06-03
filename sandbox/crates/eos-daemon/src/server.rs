//! The async RPC server: `AF_UNIX` + loopback-TCP listeners, framing, shutdown.
//!
//! This is the primary daemon tokio surface. It listens on an `AF_UNIX`
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
//!    ring + invocation registry use synchronous mutexes held only across
//!    non-await sections.
//! 2. **One OCC writer per root.** Write-capable handlers run inside their
//!    per-request dispatch task and route to the dispatcher-owned per-root
//!    `OccService` cache. The server never holds a mutex guard across an await
//!    point while doing that dispatch.
//!
//! Shutdown is driven by a [`tokio_util::sync::CancellationToken`]: a SIGTERM /
//! SIGINT cancels it, the serve loops select on it, in-flight pipelines are
//! drained, and (per the Python `start_new_session=True`) the cancel path kills
//! the full child process group.

use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use eos_protocol::{
    audit::{build_event, Lane, ToolCallSection},
    decode, encode, Envelope, ErrorKind, Request,
};

use crate::audit_buffer::{safe_emit, AuditBuffer};
use crate::dispatcher::{DispatchContext, OpTable};
use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;

/// Maximum bytes read for a single request line (re-exported for the listener
/// buffer cap).
pub const MAX_REQUEST_BYTES: usize = eos_protocol::MAX_REQUEST_BYTES;

/// Per-request read timeout in seconds.
pub const REQUEST_READ_TIMEOUT_S: f64 = eos_protocol::REQUEST_READ_TIMEOUT_S;

/// Where the daemon binds + writes its pid, plus the optional TCP listener.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `AF_UNIX` socket path (chmod 0o600 after bind).
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

/// The running daemon: the op table, audit ring, invocation registry, and the
/// shutdown token.
///
/// It ORCHESTRATES but NEVER enters a namespace: namespace work is delegated to
/// the `eosd ns-holder` / `eosd ns-runner` children it spawns; the daemon stays
/// multi-threaded (tokio) and would fail `unshare(CLONE_NEWUSER)` / `setns` into
/// a userns itself.
pub struct DaemonServer {
    config: ServerConfig,
    op_table: OpTable,
    audit: AuditBuffer,
    invocation_registry: Arc<InFlightRegistry>,
    shutdown: CancellationToken,
}

impl DaemonServer {
    /// Assemble a daemon over `config`, wiring the op table, audit ring, the
    /// invocation registry, and the shutdown token.
    #[must_use]
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            op_table: OpTable::with_builtins(),
            audit: AuditBuffer::new(),
            invocation_registry: Arc::new(InFlightRegistry::from_env()),
            shutdown: CancellationToken::new(),
        }
    }

    /// The shutdown token; cancel it to drain + tear down the serve loops.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Bind the `AF_UNIX` (and optional TCP) listeners, write the pid file, install
    /// the SIGTERM/SIGINT handlers, and serve until the shutdown token fires.
    ///
    /// On shutdown: cancel the serve tasks, remove the pid file, and unlink the
    /// socket.
    ///
    /// # Errors
    ///
    /// Returns an error when listener binding, pid-file setup, signal handling,
    /// request dispatch, or shutdown cleanup fails.
    pub async fn serve(self) -> Result<(), DaemonError> {
        let shutdown = self.shutdown.clone();
        let server = Arc::new(self);
        let _reaper_task = {
            let registry = Arc::clone(&server.invocation_registry);
            let shutdown = server.shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_secs_f64(registry.reaper_interval_s())) => {
                            registry.ttl_sweep();
                        }
                    }
                }
            })
        };
        let _isolated_ttl_task = {
            let shutdown = server.shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(500)) => {
                            let _ = tokio::task::spawn_blocking(crate::isolated::ttl_sweep).await;
                        }
                    }
                }
            })
        };
        // Command-session reaper (sense-2 §2.4): timeout backstop + finalize of
        // unpolled child exits. Runs in a blocking task (try_wait/killpg/fs).
        let _command_session_reaper = {
            let shutdown = server.shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {
                            let _ = tokio::task::spawn_blocking(
                                crate::command::command_session_reaper_sweep,
                            )
                            .await;
                        }
                    }
                }
            })
        };
        // Reap stale command sessions left by a prior daemon, before accepting.
        crate::command::recover_orphaned_command_sessions();
        let _ = (&server.audit, &server.invocation_registry);

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
                        () = server.shutdown.cancelled() => break,
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
                            () = server.shutdown.cancelled() => break,
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
            () = shutdown.cancelled() => {}
            () = signal_shutdown() => shutdown.cancel(),
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
    async fn handle_connection<S>(&self, stream: S, is_tcp: bool) -> Result<(), DaemonError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let bytes = read_request_line(&mut reader).await;
        let response = match bytes {
            Ok(bytes) => self.dispatch_bytes(bytes, is_tcp).await,
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

    async fn dispatch_bytes(&self, bytes: Vec<u8>, is_tcp: bool) -> serde_json::Value {
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
            Ok(Envelope::Request(request)) => self.dispatch_request(request).await,
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

    async fn dispatch_request(&self, request: Request) -> serde_json::Value {
        let invocation_id = request.invocation_id.clone();
        let agent_id = agent_id_from_args(&request.args);
        let background = request
            .args
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let op = request.op.clone();
        let emit_tool_events = should_emit_tool_call_event(&op);
        let started = Instant::now();
        if emit_tool_events {
            emit_tool_call_event(
                "tool_call.started",
                &invocation_id,
                &op,
                &agent_id,
                None,
                None,
            );
        }
        let table = self.op_table.clone();
        let registry = Arc::clone(&self.invocation_registry);
        let task_invocation_id = invocation_id.clone();
        let task_registry = Arc::clone(&registry);
        let (start_tx, start_rx) = std_mpsc::channel::<()>();
        let task = tokio::task::spawn_blocking(move || {
            let _ = start_rx.recv();
            let _active_call = task_registry.enter_call(&task_invocation_id);
            table.dispatch_with_context(
                &request,
                DispatchContext::with_invocation_registry(&task_registry),
            )
        });
        registry.register(
            &invocation_id,
            task.abort_handle(),
            &agent_id,
            &op,
            background,
        );
        let _ = start_tx.send(());
        let response = match task.await {
            Ok(response) => response,
            Err(err) if err.is_cancelled() => crate::dispatcher::error_envelope(
                ErrorKind::InternalError,
                "daemon invocation cancelled",
                serde_json::json!({"op": op}),
            ),
            Err(err) => crate::dispatcher::error_envelope(
                ErrorKind::InternalError,
                &format!("daemon invocation failed: {err}"),
                serde_json::json!({"op": op}),
            ),
        };
        registry.deregister(&invocation_id);
        if emit_tool_events {
            emit_tool_call_event(
                "tool_call.finished",
                &invocation_id,
                &op,
                &agent_id,
                Some(started.elapsed().as_secs_f64() * 1000.0),
                response_status(&response),
            );
        }
        response
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

fn should_emit_tool_call_event(op: &str) -> bool {
    !op.starts_with("api.audit.")
        && !matches!(
            op,
            "api.v1.heartbeat" | "api.v1.inflight_count" | "api.v1.command_session_count"
        )
}

fn emit_tool_call_event(
    event_type: &str,
    invocation_id: &str,
    op: &str,
    agent_id: &str,
    total_ms: Option<f64>,
    exit_status: Option<String>,
) {
    let section = ToolCallSection {
        tool_use_id: invocation_id.to_owned(),
        tool_name: op.to_owned(),
        agent_id: (!agent_id.is_empty()).then(|| agent_id.to_owned()),
        workspace_mode: None,
        workspace_handle_id: None,
        phase: None,
        duration_ms: None,
        total_ms,
        exit_status,
        bytes_in: None,
        bytes_out: None,
        phase_totals_rollup: None,
    };
    if let Ok(section) = serde_json::to_value(section) {
        safe_emit(build_event(event_type, "tool_call", section), Lane::Normal);
    }
}

fn response_status(response: &serde_json::Value) -> Option<String> {
    if response.get("success").and_then(serde_json::Value::as_bool) == Some(false)
        || response.get("error").is_some_and(|error| !error.is_null())
    {
        return Some("error".to_owned());
    }
    response
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| Some("ok".to_owned()))
}

fn agent_id_from_args(args: &serde_json::Value) -> String {
    args.get("agent_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
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
