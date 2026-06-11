//! Async RPC server: `AF_UNIX` plus optional loopback TCP, one framed request per
//! connection, dispatch through [`crate::dispatcher::OpTable`], and token-driven
//! shutdown. Connection handlers keep mutex guards out of await points.

use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::wire::{decode_value, encode, Envelope, ErrorKind, Request};
use eos_config::configs::{
    daemon::{DaemonConfig, FileLimitsConfig},
    isolated_workspace::IsolatedWorkspaceConfig,
};
use eos_isolated_workspace::CurrentExeNsRunnerLauncher;
use eos_runtime::{maintenance::sweepers, RuntimeServices};

use crate::dispatcher::OpTable;
use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;
use crate::request_args::trimmed_string;
use crate::DispatchContext;

const MAX_REQUEST_BYTES: usize = crate::wire::MAX_REQUEST_BYTES;
const REQUEST_READ_TIMEOUT_S: f64 = crate::wire::REQUEST_READ_TIMEOUT_S;

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

/// The running daemon: op table, runtime services, invocation registry, and
/// shutdown token.
pub struct DaemonServer {
    config: ServerConfig,
    op_table: Arc<OpTable>,
    services: Arc<RuntimeServices>,
    file_limits: FileLimitsConfig,
    invocation_registry: Arc<InFlightRegistry>,
    isolated_sweeper_interval_ms: u64,
    shutdown: CancellationToken,
}

impl DaemonServer {
    /// Assemble a daemon over `config`, wiring the op table, the owned
    /// services, the invocation registry, and the shutdown token.
    #[must_use]
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            op_table: Arc::new(OpTable::with_builtins()),
            services: Arc::new(RuntimeServices::new(
                eos_config::configs::daemon::PluginRuntimeConfig::default(),
                IsolatedWorkspaceConfig::default(),
                Arc::new(CurrentExeNsRunnerLauncher),
            )),
            file_limits: FileLimitsConfig {
                max_read_bytes: eos_config::configs::daemon::MAX_READ_BYTES,
                max_write_bytes: eos_config::configs::daemon::MAX_FILE_BYTES,
            },
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
        eos_command_ops::configure_command_sessions(&daemon_config.command_sessions);
        eos_layerstack::configure_auto_squash_max_depth(
            daemon_config.layer_stack.auto_squash_max_depth,
        );
        Self {
            config,
            op_table: Arc::new(OpTable::with_builtins()),
            services: Arc::new(RuntimeServices::new(
                daemon_config.plugin.clone(),
                isolated_config.clone(),
                Arc::new(CurrentExeNsRunnerLauncher),
            )),
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
            let services = Arc::clone(&server.services);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(sweep_interval_ms)) => {
                            let services = Arc::clone(&services);
                            let _ = tokio::task::spawn_blocking(move || {
                                sweepers::sweep_workspace_ttl(&services.workspace)
                            })
                            .await;
                        }
                    }
                }
            })
        };
        // Command-session reaping can touch process state and the filesystem.
        let _command_session_reaper = {
            let shutdown = server.shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {
                            let _ = tokio::task::spawn_blocking(
                                sweepers::sweep_command_sessions,
                            )
                            .await;
                        }
                    }
                }
            })
        };
        // Reap stale command sessions left by a prior daemon, before accepting.
        sweepers::recover_orphaned_command_sessions();

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
                    &crate::wire::ProtocolError::from(err).to_string(),
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
        let caller_id = trimmed_string(&request.args, "caller_id");
        let background = request
            .args
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let op = request.op.clone();
        let table = Arc::clone(&self.op_table);
        let registry = Arc::clone(&self.invocation_registry);
        let task_registry = Arc::clone(&registry);
        let task_services = Arc::clone(&self.services);
        let file_limits = self.file_limits;
        let (start_tx, start_rx) = std_mpsc::channel::<()>();
        let task = tokio::task::spawn_blocking(move || {
            let _ = start_rx.recv();
            table.dispatch_with_context(
                &request,
                DispatchContext::with_runtime_config(
                    &task_services,
                    &task_registry,
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
            .and_then(|object| object.remove(crate::wire::DAEMON_AUTH_FIELD))
            .and_then(|value| value.as_str().map(str::to_owned));
        if token.as_deref() != Some(expected) {
            return Err(DaemonError::Unauthorized);
        }
        Ok(value)
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

async fn signal_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
