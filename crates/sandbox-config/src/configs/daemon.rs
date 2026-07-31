//! Typed schema for the daemon section of `eos-sandbox/config/prd.yml`.
//!
//! The `sandbox-daemon` binary loads this section from the merged sandbox YAML
//! and injects it into daemon-owned subsystems during server startup. The
//! request limits map into the `sandbox-protocol` `ProtocolLimits` value type
//! at startup; the protocol crate never imports this one.

use std::path::PathBuf;

use serde::{de, Deserialize, Deserializer};

use crate::configs::validate::{
    require_f64_gt, require_unix_absolute, require_usize_at_least, require_usize_at_most,
    ConfigFieldError,
};

pub const MAX_DAEMON_WORKER_THREADS: usize = 64;
pub const MAX_DAEMON_BLOCKING_THREADS: usize = 256;
pub const MAX_DAEMON_CONNECTIONS: usize = 4096;
pub const MAX_BLOCKING_THREAD_KEEP_ALIVE_S: f64 = 300.0;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    #[serde(default)]
    pub http: DaemonHttpConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DaemonServerConfig {
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    /// Exact Tokio worker count selected for this daemon profile.
    pub worker_threads: usize,
    /// Maximum number of Tokio blocking-pool threads.
    pub max_blocking_threads: usize,
    /// Seconds an idle blocking-pool thread remains alive.
    pub blocking_thread_keep_alive_s: f64,
    /// RPC connection-permit count (both listeners share one semaphore).
    pub max_concurrent_connections: usize,
    /// Request envelope byte cap, enforced on the RPC read path and the HTTP
    /// API body.
    pub max_request_bytes: usize,
    /// Deadline for reading one request line off an accepted connection.
    pub request_read_timeout_s: f64,
    /// Deadline for writing and closing one response line to a connected peer.
    pub response_write_timeout_s: f64,
    #[doc(hidden)]
    pub legacy_worker_threads_alias_used: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonServerConfigDocument {
    socket_path: PathBuf,
    pid_path: PathBuf,
    worker_threads: Option<usize>,
    max_worker_threads: Option<usize>,
    #[serde(default = "default_max_blocking_threads")]
    max_blocking_threads: usize,
    #[serde(default = "default_blocking_thread_keep_alive_s")]
    blocking_thread_keep_alive_s: f64,
    #[serde(default = "default_max_concurrent_connections")]
    max_concurrent_connections: usize,
    #[serde(default = "default_max_request_bytes")]
    max_request_bytes: usize,
    #[serde(default = "default_request_read_timeout_s")]
    request_read_timeout_s: f64,
    #[serde(default = "default_response_write_timeout_s")]
    response_write_timeout_s: f64,
}

impl<'de> Deserialize<'de> for DaemonServerConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let document = DaemonServerConfigDocument::deserialize(deserializer)?;
        let (worker_threads, legacy_worker_threads_alias_used) = match (
            document.worker_threads,
            document.max_worker_threads,
        ) {
            (Some(_), Some(_)) => {
                return Err(de::Error::custom(
                        "daemon.server.worker_threads and deprecated daemon.server.max_worker_threads cannot both be set",
                    ));
            }
            (Some(value), None) => (value, false),
            (None, Some(value)) => (value, true),
            (None, None) => return Err(de::Error::missing_field("worker_threads")),
        };

        Ok(Self {
            socket_path: document.socket_path,
            pid_path: document.pid_path,
            worker_threads,
            max_blocking_threads: document.max_blocking_threads,
            blocking_thread_keep_alive_s: document.blocking_thread_keep_alive_s,
            max_concurrent_connections: document.max_concurrent_connections,
            max_request_bytes: document.max_request_bytes,
            request_read_timeout_s: document.request_read_timeout_s,
            response_write_timeout_s: document.response_write_timeout_s,
            legacy_worker_threads_alias_used,
        })
    }
}

impl DaemonServerConfig {
    #[must_use]
    pub fn used_legacy_worker_threads(&self) -> bool {
        self.legacy_worker_threads_alias_used
    }
}

/// Daemon HTTP surface tuning (`daemon.http`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonHttpConfig {
    pub forward: DaemonHttpForwardConfig,
}

/// `/forward` reverse-proxy deadlines (`daemon.http.forward`).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonHttpForwardConfig {
    /// TCP connect deadline toward the forward target.
    pub connect_timeout_s: f64,
    /// Upstream response deadline once connected.
    pub response_timeout_s: f64,
}

impl Default for DaemonHttpForwardConfig {
    fn default() -> Self {
        Self {
            connect_timeout_s: 10.0,
            response_timeout_s: 30.0,
        }
    }
}

impl DaemonConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates daemon runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_unix_absolute(&self.server.socket_path, "daemon.server.socket_path")?;
        require_unix_absolute(&self.server.pid_path, "daemon.server.pid_path")?;
        require_usize_at_least(
            self.server.worker_threads,
            1,
            "daemon.server.worker_threads",
        )?;
        require_usize_at_most(
            self.server.worker_threads,
            MAX_DAEMON_WORKER_THREADS,
            "daemon.server.worker_threads",
        )?;
        require_usize_at_least(
            self.server.max_blocking_threads,
            1,
            "daemon.server.max_blocking_threads",
        )?;
        require_usize_at_most(
            self.server.max_blocking_threads,
            MAX_DAEMON_BLOCKING_THREADS,
            "daemon.server.max_blocking_threads",
        )?;
        require_f64_gt(
            self.server.blocking_thread_keep_alive_s,
            0.0,
            "daemon.server.blocking_thread_keep_alive_s",
        )?;
        if self.server.blocking_thread_keep_alive_s > MAX_BLOCKING_THREAD_KEEP_ALIVE_S {
            return Err(ConfigFieldError::new(
                "daemon.server.blocking_thread_keep_alive_s",
                format!("must be at most {MAX_BLOCKING_THREAD_KEEP_ALIVE_S}"),
            ));
        }
        require_usize_at_least(
            self.server.max_concurrent_connections,
            1,
            "daemon.server.max_concurrent_connections",
        )?;
        require_usize_at_most(
            self.server.max_concurrent_connections,
            MAX_DAEMON_CONNECTIONS,
            "daemon.server.max_concurrent_connections",
        )?;
        require_usize_at_least(
            self.server.max_request_bytes,
            65536,
            "daemon.server.max_request_bytes",
        )?;
        require_f64_gt(
            self.server.request_read_timeout_s,
            0.0,
            "daemon.server.request_read_timeout_s",
        )?;
        require_f64_gt(
            self.server.response_write_timeout_s,
            0.0,
            "daemon.server.response_write_timeout_s",
        )?;
        require_f64_gt(
            self.http.forward.connect_timeout_s,
            0.0,
            "daemon.http.forward.connect_timeout_s",
        )?;
        require_f64_gt(
            self.http.forward.response_timeout_s,
            0.0,
            "daemon.http.forward.response_timeout_s",
        )?;
        Ok(())
    }
}

fn default_max_concurrent_connections() -> usize {
    64
}

fn default_max_blocking_threads() -> usize {
    8
}

fn default_blocking_thread_keep_alive_s() -> f64 {
    5.0
}

fn default_max_request_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_request_read_timeout_s() -> f64 {
    30.0
}

fn default_response_write_timeout_s() -> f64 {
    30.0
}
