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
//! drained, and cancellation kills the full child process group.

use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio_util::sync::CancellationToken;

use eos_config::configs::{
    daemon::{AuditConfig, DaemonConfig, FileLimitsConfig},
    isolated_workspace::IsolatedWorkspaceConfig,
};
use eos_protocol::{decode_value, encode, Envelope, ErrorKind, Request};

use super::framing::{read_request_line, signal_shutdown, MAX_REQUEST_BYTES};
use super::tool_call_events::{caller_id_from_args, emit_tool_call_event};
use crate::audit::events::should_emit_tool_call_event;
use crate::dispatcher::{DispatchContext, OpTable};
use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;

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

/// The running daemon: the op table, audit config, invocation registry, and the
/// shutdown token. Audit events flow through the process-wide audit ring
/// singleton (`audit::buffer`), not an instance field.
///
/// It ORCHESTRATES but NEVER enters a namespace: namespace work is delegated to
/// the `eosd ns-holder` / `eosd ns-runner` children it spawns; the daemon stays
/// multi-threaded (tokio) and would fail `unshare(CLONE_NEWUSER)` / `setns` into
/// a userns itself.
pub struct DaemonServer {
    config: ServerConfig,
    op_table: Arc<OpTable>,
    audit_config: AuditConfig,
    file_limits: FileLimitsConfig,
    invocation_registry: Arc<InFlightRegistry>,
    isolated_sweeper_interval_ms: u64,
    shutdown: CancellationToken,
}

impl DaemonServer {
    /// Assemble a daemon over `config`, wiring the op table, audit ring, the
    /// invocation registry, and the shutdown token.
    #[must_use]
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            op_table: Arc::new(OpTable::with_builtins()),
            audit_config: default_audit_config(),
            file_limits: default_file_limits(),
            invocation_registry: Arc::new(InFlightRegistry::new(
                crate::DEFAULT_TTL_S,
                crate::DEFAULT_REAPER_INTERVAL_S,
            )),
            isolated_sweeper_interval_ms: 500,
            shutdown: CancellationToken::new(),
        }
    }

    /// Assemble a daemon using the typed `daemon` config section loaded from
    /// `sandbox/config/prd.yml`.
    #[must_use]
    pub fn with_daemon_config(
        config: ServerConfig,
        daemon_config: &DaemonConfig,
        isolated_config: &IsolatedWorkspaceConfig,
    ) -> Self {
        crate::adapters::workspace_run::configure_command_sessions(&daemon_config.command_sessions);
        crate::adapters::workspace_run::isolated::configure_isolated_workspace(isolated_config);
        crate::adapters::plugins::configure_plugin_runtime(&daemon_config.plugin);
        crate::adapters::occ::configure_layer_stack(&daemon_config.layer_stack);
        Self {
            config,
            op_table: Arc::new(OpTable::with_builtins()),
            audit_config: daemon_config.audit.clone(),
            file_limits: daemon_config.files,
            invocation_registry: Arc::new(InFlightRegistry::new(
                daemon_config.inflight.ttl_s,
                daemon_config.inflight.reaper_interval_s,
            )),
            isolated_sweeper_interval_ms: daemon_config.isolated_sweeper.ttl_sweep_interval_ms,
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
            let sweep_interval_ms = server.isolated_sweeper_interval_ms;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(sweep_interval_ms)) => {
                            let _ = tokio::task::spawn_blocking(crate::adapters::workspace_run::isolated::ttl_sweep).await;
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
                                crate::adapters::workspace_run::command_session_reaper_sweep,
                            )
                            .await;
                        }
                    }
                }
            })
        };
        // Reap stale command sessions left by a prior daemon, before accepting.
        crate::adapters::workspace_run::recover_orphaned_command_sessions();

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
        let read_start = Instant::now();
        let bytes = read_request_line(&mut reader).await;
        let read_request_s = read_start.elapsed().as_secs_f64();
        let response = match bytes {
            Ok(bytes) => self.dispatch_bytes(bytes, is_tcp, read_request_s).await,
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

    async fn dispatch_bytes(
        &self,
        bytes: Vec<u8>,
        is_tcp: bool,
        read_request_s: f64,
    ) -> serde_json::Value {
        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                return crate::dispatcher::error_envelope(
                    ErrorKind::BadJson,
                    &eos_protocol::ProtocolError::from(err).to_string(),
                    serde_json::json!({}),
                );
            }
        };
        let value = if is_tcp {
            match self.strip_tcp_auth(value) {
                Ok(value) => value,
                Err(err) => {
                    return crate::dispatcher::error_envelope(
                        err.wire_kind(),
                        &err.to_string(),
                        serde_json::json!({}),
                    );
                }
            }
        } else {
            value
        };
        match decode_value(value) {
            Ok(Envelope::Request(request)) => self.dispatch_request(request, read_request_s).await,
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

    async fn dispatch_request(&self, request: Request, read_request_s: f64) -> serde_json::Value {
        let invocation_id = request.invocation_id.clone();
        let caller_id = caller_id_from_args(&request.args);
        let background = request
            .args
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let op = request.op.clone();
        let emit_tool_events = should_emit_tool_call_event(&op);
        if emit_tool_events {
            emit_tool_call_event(
                "tool_call.started",
                &invocation_id,
                &op,
                &caller_id,
                None,
                None,
            );
        }
        let table = Arc::clone(&self.op_table);
        let registry = Arc::clone(&self.invocation_registry);
        let task_registry = Arc::clone(&registry);
        let audit_config = self.audit_config.clone();
        let file_limits = self.file_limits;
        let (start_tx, start_rx) = std_mpsc::channel::<()>();
        let task = tokio::task::spawn_blocking(move || {
            let _ = start_rx.recv();
            table.dispatch_with_context(
                &request,
                DispatchContext::with_runtime_config(
                    &task_registry,
                    &audit_config,
                    file_limits,
                    read_request_s,
                ),
            )
        });
        registry.register(&invocation_id, task.abort_handle(), &caller_id, background);
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
        // The rich `tool_call.completed` is emitted once by the dispatcher's
        // audit pass (see `audit::events::emit_dispatch_audit`); the transport
        // layer only opens the lifecycle with `tool_call.started` so a single
        // `tool_call.completed` lands per op instead of two on `Lane::Normal`.
        response
    }

    fn strip_tcp_auth(
        &self,
        mut value: serde_json::Value,
    ) -> Result<serde_json::Value, DaemonError> {
        let Some(expected) = self
            .config
            .auth_token
            .as_deref()
            .filter(|token| !token.is_empty())
        else {
            return Ok(value);
        };
        let token = value
            .as_object_mut()
            .and_then(|object| object.remove(eos_protocol::DAEMON_AUTH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));
        if token.as_deref() != Some(expected) {
            return Err(DaemonError::Unauthorized);
        }
        Ok(value)
    }
}
fn default_file_limits() -> FileLimitsConfig {
    FileLimitsConfig {
        max_read_bytes: eos_protocol::models::MAX_READ_BYTES,
        max_write_bytes: eos_protocol::models::MAX_FILE_BYTES,
    }
}

fn default_audit_config() -> AuditConfig {
    AuditConfig {
        allow_floor_reset: false,
        pull_limit_default: 1000,
        ring_max_events: 50_000,
        ring_max_bytes: 8_388_608,
        pressure_threshold: 0.8,
    }
}
