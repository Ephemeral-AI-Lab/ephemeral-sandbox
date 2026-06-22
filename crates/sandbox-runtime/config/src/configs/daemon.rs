//! Typed schema for the daemon section of `eos-sandbox/config/prd.yml`.
//!
//! The `sandbox-daemon` binary loads this section from the merged runtime YAML
//! and injects it into daemon-owned subsystems during server startup.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_non_empty, require_u64_at_least, require_usize_at_least,
    ConfigFieldError,
};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    pub commands: CommandConfig,
    pub cgroup_monitor: CgroupMonitorConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    pub idle_workspace_eviction: IdleWorkspaceEvictionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    pub scratch_root: PathBuf,
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            scratch_root: PathBuf::from("/eos/scratch/commands"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupMonitorConfig {
    pub enabled: bool,
    pub sample_interval_ms: u64,
    pub retained_samples_per_target: usize,
    pub include_pids: bool,
    pub include_pressure: bool,
    pub include_disk: bool,
}

impl Default for CgroupMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_interval_ms: 1000,
            retained_samples_per_target: 100,
            include_pids: true,
            include_pressure: true,
            include_disk: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub level: String,
    #[serde(default)]
    pub sink: Option<TelemetrySink>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: "sandbox-daemon".to_owned(),
            level: "info".to_owned(),
            sink: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TelemetrySink {
    LocalJson { stream: TelemetryOutputStream },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonServeMode {
    Foreground,
    Spawn,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonServerConfig {
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub max_worker_threads: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdleWorkspaceEvictionConfig {
    pub interval_ms: u64,
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
        require_absolute(&self.commands.scratch_root, "daemon.commands.scratch_root")?;
        reject_dangerous_root(&self.commands.scratch_root, "daemon.commands.scratch_root")?;
        require_u64_at_least(
            self.cgroup_monitor.sample_interval_ms,
            1,
            "daemon.cgroup_monitor.sample_interval_ms",
        )?;
        require_usize_at_least(
            self.cgroup_monitor.retained_samples_per_target,
            1,
            "daemon.cgroup_monitor.retained_samples_per_target",
        )?;
        self.telemetry.validate()?;
        require_u64_at_least(
            self.idle_workspace_eviction.interval_ms,
            1,
            "daemon.idle_workspace_eviction.interval_ms",
        )?;
        Ok(())
    }
}

impl TelemetryConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when the telemetry config is internally inconsistent.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_non_empty(&self.service_name, "daemon.telemetry.service_name")?;
        validate_telemetry_level(&self.level)?;
        if self.enabled && self.sink.is_none() {
            return Err(ConfigFieldError::new(
                "daemon.telemetry.sink",
                "enabled telemetry requires exactly one sink",
            ));
        }
        Ok(())
    }

    /// Validate serve-mode constraints for daemon-owned telemetry sinks.
    ///
    /// # Errors
    /// Returns an error when the configured sink cannot run in `mode`.
    pub fn validate_for_serve_mode(&self, mode: DaemonServeMode) -> Result<(), ConfigFieldError> {
        self.validate()?;
        if !self.enabled {
            return Ok(());
        }
        if matches!(
            (mode, &self.sink),
            (
                DaemonServeMode::Spawn,
                Some(TelemetrySink::LocalJson { .. })
            )
        ) {
            return Err(ConfigFieldError::new(
                "daemon.telemetry.sink",
                "local_json stdout/stderr telemetry requires foreground serve mode",
            ));
        }
        Ok(())
    }
}

fn validate_telemetry_level(level: &str) -> Result<(), ConfigFieldError> {
    match level {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(()),
        _ => Err(ConfigFieldError::new(
            "daemon.telemetry.level",
            "must be one of trace, debug, info, warn, error",
        )),
    }
}

fn reject_dangerous_root(
    path: &std::path::Path,
    field: &'static str,
) -> Result<(), ConfigFieldError> {
    if is_filesystem_root(path) {
        return Err(ConfigFieldError::new(
            field,
            "must not be the filesystem root",
        ));
    }
    Ok(())
}

fn is_filesystem_root(path: &std::path::Path) -> bool {
    path.parent().is_none()
        || path
            .canonicalize()
            .ok()
            .is_some_and(|canonical| canonical.parent().is_none())
}
