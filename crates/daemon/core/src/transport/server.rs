//! Async RPC server: `AF_UNIX` plus optional loopback TCP, one framed request per
//! connection, dispatch through the daemon dispatcher, and token-driven
//! shutdown. Connection handlers keep mutex guards out of await points.

mod connection;
mod dispatch;
mod lifecycle;
mod trace_context;

use std::path::PathBuf;
use std::sync::Arc;

use config::configs::{
    daemon::{DaemonConfig, FileLimitsConfig},
    isolated_workspace::IsolatedWorkspaceConfig,
};
use tokio_util::sync::CancellationToken;
use workspace::CurrentExeNsRunnerLauncher;

use crate::invocation_registry::InFlightRegistry;
use crate::RuntimeServices;

const MAX_REQUEST_BYTES: usize = crate::wire::MAX_REQUEST_BYTES;
#[cfg(not(test))]
const REQUEST_READ_TIMEOUT_S: f64 = crate::wire::REQUEST_READ_TIMEOUT_S;
#[cfg(test)]
const REQUEST_READ_TIMEOUT_S: f64 = 0.1;

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
    /// Host-forward TCP auth token; permits host-only and operator daemon ops.
    pub forward_auth_token: Option<String>,
}

/// The running daemon: op table, runtime services, invocation registry, and
/// shutdown token.
pub struct DaemonServer {
    config: ServerConfig,
    services: Arc<RuntimeServices>,
    file_limits: FileLimitsConfig,
    invocation_registry: Arc<InFlightRegistry>,
    idle_workspace_eviction_interval_ms: u64,
    shutdown: CancellationToken,
}

impl DaemonServer {
    /// Assemble a daemon over `config`, wiring the op table, the owned
    /// services, the invocation registry, and the shutdown token.
    #[must_use]
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            services: Arc::new(RuntimeServices::new(
                config::configs::daemon::PluginRuntimeConfig::default(),
                IsolatedWorkspaceConfig::default(),
                command::CommandConfig::default(),
                Arc::new(CurrentExeNsRunnerLauncher),
            )),
            file_limits: FileLimitsConfig {
                max_read_bytes: config::configs::daemon::MAX_READ_BYTES,
                max_write_bytes: config::configs::daemon::MAX_FILE_BYTES,
            },
            invocation_registry: Arc::new(InFlightRegistry::new(
                crate::DEFAULT_TTL_S,
                crate::DEFAULT_REAPER_INTERVAL_S,
            )),
            idle_workspace_eviction_interval_ms: 500,
            shutdown: CancellationToken::new(),
        }
    }

    /// Assemble a daemon using the typed `daemon` config section loaded from
    /// `eos-sandbox/config/prd.yml`.
    #[must_use]
    pub fn with_daemon_config(
        config: ServerConfig,
        daemon_config: &DaemonConfig,
        isolated_config: &IsolatedWorkspaceConfig,
    ) -> Self {
        Self {
            config,
            services: Arc::new(RuntimeServices::with_commit_options(
                daemon_config.plugin.clone(),
                isolated_config.clone(),
                daemon_config.commands.clone(),
                Arc::new(CurrentExeNsRunnerLauncher),
                layerstack::CommitOptions::new(daemon_config.layer_stack.auto_squash_max_depth),
            )),
            file_limits: daemon_config.files,
            invocation_registry: Arc::new(InFlightRegistry::new(
                daemon_config.inflight.ttl_s,
                daemon_config.inflight.reaper_interval_s,
            )),
            idle_workspace_eviction_interval_ms: daemon_config.idle_workspace_eviction.interval_ms,
            shutdown: CancellationToken::new(),
        }
    }

    /// The shutdown token; cancel it to drain + tear down the serve loops.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}
