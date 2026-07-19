use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::task::TaskTracker;

use super::{AdmissionError, ConnectionAdmission, SandboxDaemonServer};
use crate::rpc::error::SandboxDaemonError;

impl SandboxDaemonServer {
    /// Bind the `AF_UNIX` (and optional TCP) listeners, write the pid file, install
    /// the Ctrl-C handler, and serve until the shutdown token fires.
    ///
    /// Shutdown closes admission first, stops listeners, drains accepted
    /// connections, joins the synchronous runtime teardown, and finally
    /// removes the pid file and socket. Runtime and artifact failures are
    /// reported only after every cleanup phase has been attempted.
    ///
    /// # Errors
    ///
    /// Returns an error when listener binding, pid-file setup, signal handling,
    /// request dispatch, or shutdown cleanup fails.
    pub async fn serve(self) -> Result<(), SandboxDaemonError> {
        let shutdown = self.shutdown.clone();
        let connection_tasks = self.async_tasks.clone();
        let socket_path = self.config.socket_path.clone();
        let pid_path = self.config.pid_path.clone();
        let server = Arc::new(self);

        let listener_result = serve_listeners(Arc::clone(&server), connection_tasks.clone()).await;

        // Repeat this boundary outside `serve_listeners` so startup failures
        // (before any listener task exists) follow the identical close path.
        server.connection_admission.close();
        server.blocking_admission.close();
        shutdown.cancel();
        drain_connection_tasks(&connection_tasks).await;

        let operations = Arc::clone(&server.operations);
        let runtime_result = tokio::task::spawn_blocking(move || operations.shutdown()).await;
        let artifact_result = remove_run_artifacts(&pid_path, &socket_path).await;

        let mut cleanup_failures = Vec::new();
        match runtime_result {
            Ok(report) if report.is_complete() => {}
            Ok(report) => cleanup_failures.push(format!(
                "runtime shutdown incomplete: sessions_converged={}/{}, failures={}",
                report.sessions_converged,
                report.sessions_observed,
                report.failures.len()
            )),
            Err(error) => cleanup_failures.push(format!("runtime shutdown task failed: {error}")),
        }
        if let Err(error) = artifact_result {
            cleanup_failures.push(format!("run artifact cleanup failed: {error}"));
        }

        match (listener_result, cleanup_failures.is_empty()) {
            (Ok(()), true) => Ok(()),
            (Err(error), true) => Err(error),
            (result, false) => {
                if let Err(error) = result {
                    cleanup_failures.insert(0, error.to_string());
                }
                Err(SandboxDaemonError::Lifecycle {
                    message: cleanup_failures.join("; "),
                })
            }
        }
    }
}

async fn serve_listeners(
    server: Arc<SandboxDaemonServer>,
    connection_tasks: TaskTracker,
) -> Result<(), SandboxDaemonError> {
    let shutdown = server.shutdown.clone();
    let resource_tasks = TaskTracker::new();
    if let Some(parent) = server.config.socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = server.config.pid_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&server.config.socket_path).await;
    let unix_listener = UnixListener::bind(&server.config.socket_path)?;
    tokio::fs::set_permissions(
        &server.config.socket_path,
        std::fs::Permissions::from_mode(0o600),
    )
    .await?;

    // Finish every fallible bind before publishing the pid or spawning a
    // task. A startup error therefore leaves no detached listener; the
    // outer lifecycle still performs runtime teardown and exact cleanup.
    let http_listener = match server.config.http_bind() {
        Some((host, port)) => Some(TcpListener::bind((host, port)).await?),
        None => None,
    };
    let tcp_listener = match (&server.config.tcp_host, server.config.tcp_port) {
        (Some(host), Some(port)) => Some(TcpListener::bind((host.as_str(), port)).await?),
        _ => None,
    };
    tokio::fs::write(&server.config.pid_path, std::process::id().to_string()).await?;
    if let Some(observability) = &server.observability {
        observability.start_resource_sampler(&resource_tasks, shutdown.clone());
    }

    let mut unix_server = {
        let server = Arc::clone(&server);
        let connection_admission = server.connection_admission.clone();
        let connection_tasks = connection_tasks.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = server.shutdown.cancelled() => break,
                    accepted = unix_listener.accept() => {
                        let (stream, _) = accepted?;
                        let Some((stream, permit)) = admit_rpc_connection(
                            stream,
                            &connection_admission,
                            server.config.max_concurrent_connections,
                        ).await else {
                            continue;
                        };
                        let server = Arc::clone(&server);
                        connection_tasks.spawn(async move {
                            let _permit = permit;
                            let _ = server.handle_connection(stream, false, None, None).await;
                        });
                    }
                }
            }
            Ok::<(), std::io::Error>(())
        })
    };

    let mut http_server = http_listener.map(|listener| {
        crate::http::spawn(
            listener,
            server.config.clone(),
            Arc::clone(&server.operations),
            server.observer(),
            server.blocking_admission.clone(),
            server.connection_admission.clone(),
            connection_tasks.clone(),
            server.shutdown.clone(),
        )
    });

    let mut tcp_server = match tcp_listener {
        Some(listener) => {
            let server = Arc::clone(&server);
            let connection_admission = server.connection_admission.clone();
            let connection_tasks = connection_tasks.clone();
            Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = server.shutdown.cancelled() => break,
                        accepted = listener.accept() => {
                            let (stream, peer_addr) = accepted?;
                            let Some((stream, permit)) = admit_rpc_connection(
                                stream,
                                &connection_admission,
                                server.config.max_concurrent_connections,
                            ).await else {
                                continue;
                            };
                            let local_addr = stream.local_addr().ok();
                            let server = Arc::clone(&server);
                            connection_tasks.spawn(async move {
                                let _permit = permit;
                                let _ = server
                                    .handle_connection(
                                        stream,
                                        true,
                                        Some(peer_addr),
                                        local_addr,
                                    )
                                    .await;
                            });
                        }
                    }
                }
                Ok::<(), std::io::Error>(())
            }))
        }
        None => None,
    };

    let mut listener_result = tokio::select! {
        () = shutdown.cancelled() => Ok(()),
        () = signal_shutdown() => Ok(()),
        result = &mut unix_server => match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(SandboxDaemonError::Io(err)),
            Err(err) => Err(SandboxDaemonError::Io(std::io::Error::other(format!(
                "unix listener task failed: {err}"
            )))),
        },
        result = async {
            let Some(task) = tcp_server.as_mut() else {
                return std::future::pending().await;
            };
            task.await
        } => match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(SandboxDaemonError::Io(err)),
            Err(err) => Err(SandboxDaemonError::Io(std::io::Error::other(format!(
                "tcp listener task failed: {err}"
            )))),
        },
    };
    // Close both admission boundaries before waking connection handlers.
    // A request racing shutdown is therefore rejected as shutdown, never
    // misreported as ordinary capacity pressure.
    server.connection_admission.close();
    server.blocking_admission.close();
    shutdown.cancel();
    if !unix_server.is_finished() {
        unix_server.abort();
        let _ = unix_server.await;
    }
    if let Some(task) = tcp_server.as_mut() {
        if !task.is_finished() {
            task.abort();
            let _ = task.await;
        }
    }
    drop(tcp_server);
    resource_tasks.close();
    resource_tasks.wait().await;
    if let Some(task) = http_server.as_mut() {
        if let Err(error) = task.await {
            if listener_result.is_ok() {
                listener_result = Err(SandboxDaemonError::Io(std::io::Error::other(format!(
                    "http listener task failed: {error}"
                ))));
            }
        }
    }
    drop(http_server);
    listener_result
}

async fn remove_run_artifacts(
    pid_path: &std::path::Path,
    socket_path: &std::path::Path,
) -> Result<(), std::io::Error> {
    let mut failures = Vec::new();
    for (label, path) in [("pid", pid_path), ("socket", socket_path)] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{label}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(failures.join("; ")))
    }
}

/// Admit an accepted JSON-line RPC stream before its handler can be spawned.
/// Both Unix and TCP listeners use this boundary, so a saturated connection
/// limit writes one structured overload response without creating a task.
pub(crate) async fn admit_rpc_connection<S>(
    stream: S,
    connection_admission: &ConnectionAdmission,
    max_concurrent_connections: usize,
) -> Option<(S, OwnedSemaphorePermit)>
where
    S: AsyncWrite + Unpin,
{
    let permit = match connection_admission.try_acquire() {
        Ok(permit) => permit,
        Err(reason) => {
            reject_connection(stream, reason, max_concurrent_connections).await;
            return None;
        }
    };
    Some((stream, permit))
}

async fn reject_connection<S>(
    mut stream: S,
    reason: AdmissionError,
    max_concurrent_connections: usize,
) where
    S: AsyncWrite + Unpin,
{
    let response = match reason {
        AdmissionError::Capacity => super::error_response(
            "server_busy",
            "daemon is at connection capacity",
            serde_json::json!({"max_concurrent_connections": max_concurrent_connections}),
        ),
        AdmissionError::Closed => super::dispatch::server_shutting_down_response(),
    };
    let mut framed = serde_json::to_vec(&response).expect("daemon overload response serializes");
    framed.push(b'\n');
    let _ = stream.write_all(&framed).await;
    let _ = stream.shutdown().await;
}

pub(crate) async fn drain_connection_tasks(connection_tasks: &TaskTracker) {
    connection_tasks.close();
    connection_tasks.wait().await;
}

async fn signal_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
