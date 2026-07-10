//! Typed schema for the daemon section of `eos-sandbox/config/prd.yml`.
//!
//! The `sandbox-daemon` binary loads this section from the merged sandbox YAML
//! and injects it into daemon-owned subsystems during server startup.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{require_absolute, require_usize_at_least, ConfigFieldError};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    #[serde(default)]
    pub http: DaemonHttpConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonServerConfig {
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub max_worker_threads: usize,
}

/// Daemon HTTP surface tuning (`daemon.http`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonHttpConfig {
    pub export: DaemonHttpExportConfig,
}

/// Export spool stream framing (`daemon.http.export`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonHttpExportConfig {
    /// Body frame size of the spool stream response.
    pub frame_bytes: usize,
    /// Bounded frame-channel depth between the spool reader and the response.
    pub channel_frames: usize,
}

impl Default for DaemonHttpExportConfig {
    fn default() -> Self {
        Self {
            frame_bytes: 1024 * 1024,
            channel_frames: 4,
        }
    }
}

impl DaemonConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates daemon runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_absolute(&self.server.socket_path, "daemon.server.socket_path")?;
        require_absolute(&self.server.pid_path, "daemon.server.pid_path")?;
        require_usize_at_least(
            self.server.max_worker_threads,
            1,
            "daemon.server.max_worker_threads",
        )?;
        require_usize_at_least(
            self.http.export.frame_bytes,
            4096,
            "daemon.http.export.frame_bytes",
        )?;
        require_usize_at_least(
            self.http.export.channel_frames,
            1,
            "daemon.http.export.channel_frames",
        )?;
        Ok(())
    }
}
