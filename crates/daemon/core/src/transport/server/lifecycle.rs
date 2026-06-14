use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Semaphore;

use super::trace_context::unix_ms;
use super::DaemonServer;
use crate::error::DaemonError;
use crate::runtime_services::background_tasks;

const MAX_CONCURRENT_CONNECTIONS: usize = 256;

impl DaemonServer {
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
        let connection_permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
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
            let eviction_interval_ms = server.idle_workspace_eviction_interval_ms;
            let services = Arc::clone(&server.services);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(eviction_interval_ms)) => {
                            let services = Arc::clone(&services);
                            let _ = tokio::task::spawn_blocking(move || {
                                background_tasks::evict_idle_workspaces_once(&services.workspace)
                            })
                            .await;
                        }
                    }
                }
            })
        };
        // Command advancement can touch process state and the filesystem.
        let _command_advancer = {
            let shutdown = server.shutdown.clone();
            let services = Arc::clone(&server.services);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {
                            let command = Arc::clone(&services.command);
                            let _ = tokio::task::spawn_blocking(
                                move || background_tasks::advance_active_commands_once(&command),
                            )
                            .await;
                        }
                    }
                }
            })
        };
        // Recover stale commands left by a prior daemon, before accepting.
        background_tasks::recover_orphaned_commands(&server.services.command);

        if let Some(parent) = server.config.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Some(parent) = server.config.pid_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let _ = tokio::fs::remove_file(&server.config.socket_path).await;
        let unix_listener = UnixListener::bind(&server.config.socket_path)?;
        emit_boot_event(
            "listen_bound",
            serde_json::json!({
                "listener_kind": "unix",
                "socket_path": server.config.socket_path.display().to_string(),
            }),
        );
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
            let connection_permits = Arc::clone(&connection_permits);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = server.shutdown.cancelled() => break,
                        accepted = unix_listener.accept() => {
                            let (stream, _) = accepted?;
                            let Ok(permit) = Arc::clone(&connection_permits).try_acquire_owned() else {
                                continue;
                            };
                            let server = Arc::clone(&server);
                            tokio::spawn(async move {
                                let _permit = permit;
                                let _ = server.handle_connection(stream, false, None, None).await;
                            });
                        }
                    }
                }
                Ok::<(), std::io::Error>(())
            })
        };

        let mut tcp_server = match (&server.config.tcp_host, server.config.tcp_port) {
            (Some(host), Some(port)) => {
                let listener = TcpListener::bind((host.as_str(), port)).await?;
                emit_boot_event(
                    "listen_bound",
                    serde_json::json!({
                        "listener_kind": "tcp",
                        "host": host,
                        "port": port,
                    }),
                );
                let server = Arc::clone(&server);
                let connection_permits = Arc::clone(&connection_permits);
                Some(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            () = server.shutdown.cancelled() => break,
                            accepted = listener.accept() => {
                                let (stream, peer_addr) = accepted?;
                                let Ok(permit) = Arc::clone(&connection_permits).try_acquire_owned() else {
                                    continue;
                                };
                                let local_addr = stream.local_addr().ok();
                                let server = Arc::clone(&server);
                                tokio::spawn(async move {
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
            result = async {
                let Some(task) = tcp_server.as_mut() else {
                    return std::future::pending().await;
                };
                task.await
            } => match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(DaemonError::Io(err)),
                Err(err) => {
                    return Err(DaemonError::Io(std::io::Error::other(format!(
                        "tcp listener task failed: {err}"
                    ))));
                }
            },
        }
        if let Some(task) = tcp_server {
            task.abort();
        }
        let _ = tokio::fs::remove_file(&server.config.pid_path).await;
        let _ = tokio::fs::remove_file(&server.config.socket_path).await;
        Ok(())
    }
}

async fn signal_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

fn emit_boot_event(event: &str, details: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "ts_ms": unix_ms(),
            "level": "info",
            "module": "daemon.boot",
            "event": event,
            "details": details,
        })
    );
}
