//! Typed schema for the daemon section of `sandbox/config/prd.yml`.
//!
//! The `eosd` binary loads this section from the merged runtime YAML and injects
//! it into daemon-owned subsystems during server startup.

use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_f64_gt, require_ratio, require_u64_at_least, require_usize_at_least,
    ConfigFieldError,
};

pub use super::command_session::CommandSessionConfig;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    pub server: DaemonServerConfig,
    pub inflight: InflightConfig,
    pub audit: AuditConfig,
    pub command_sessions: CommandSessionConfig,
    pub isolated_sweeper: IsolatedSweeperConfig,
    pub plugin: PluginRuntimeConfig,
    pub layer_stack: LayerStackConfig,
    pub files: FileLimitsConfig,
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
pub struct InflightConfig {
    pub ttl_s: f64,
    pub reaper_interval_s: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    pub allow_floor_reset: bool,
    pub pull_limit_default: usize,
    pub ring_max_events: u64,
    pub ring_max_bytes: u64,
    pub pressure_threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedSweeperConfig {
    pub ttl_sweep_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRuntimeConfig {
    pub ppc_root: PathBuf,
    pub ppc_timeout_ms: u64,
    pub service_probe_timeout_ms: u64,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerStackConfig {
    pub auto_squash_max_depth: usize,
}

/// Per-file read/write byte caps for `read_file` / `write_file` / `edit_file`.
///
/// Each bounds a single file payload. Both must stay below the transport frame
/// limit (`eos_protocol::MAX_REQUEST_BYTES`, 16 MiB): file content travels
/// inside one request/response frame next to the JSON envelope, so values near
/// 16 MiB risk frame overflow once content is JSON-escaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLimitsConfig {
    pub max_read_bytes: usize,
    pub max_write_bytes: usize,
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
        require_f64_gt(self.inflight.ttl_s, 0.0, "daemon.inflight.ttl_s")?;
        require_f64_gt(
            self.inflight.reaper_interval_s,
            0.0,
            "daemon.inflight.reaper_interval_s",
        )?;
        require_usize_at_least(
            self.audit.pull_limit_default,
            1,
            "daemon.audit.pull_limit_default",
        )?;
        require_u64_at_least(
            self.audit.ring_max_events,
            1,
            "daemon.audit.ring_max_events",
        )?;
        require_u64_at_least(self.audit.ring_max_bytes, 1, "daemon.audit.ring_max_bytes")?;
        require_ratio(
            self.audit.pressure_threshold,
            "daemon.audit.pressure_threshold",
        )?;
        require_absolute(
            &self.command_sessions.scratch_root,
            "daemon.command_sessions.scratch_root",
        )?;
        require_u64_at_least(
            self.command_sessions.default_yield_time_ms,
            1,
            "daemon.command_sessions.default_yield_time_ms",
        )?;
        require_u64_at_least(
            self.command_sessions.default_timeout_s,
            1,
            "daemon.command_sessions.default_timeout_s",
        )?;
        require_u64_at_least(
            self.command_sessions.quiet_ms,
            1,
            "daemon.command_sessions.quiet_ms",
        )?;
        require_u64_at_least(
            self.command_sessions.cancel_wait_ms,
            1,
            "daemon.command_sessions.cancel_wait_ms",
        )?;
        require_u64_at_least(
            self.command_sessions.output_drain_grace_ms,
            1,
            "daemon.command_sessions.output_drain_grace_ms",
        )?;
        require_u64_at_least(
            self.command_sessions.max_session_s,
            1,
            "daemon.command_sessions.max_session_s",
        )?;
        require_usize_at_least(
            self.command_sessions.output_ring_max_bytes,
            1,
            "daemon.command_sessions.output_ring_max_bytes",
        )?;
        require_u64_at_least(
            self.command_sessions.output_spool_max_bytes,
            1,
            "daemon.command_sessions.output_spool_max_bytes",
        )?;
        require_u64_at_least(
            self.isolated_sweeper.ttl_sweep_interval_ms,
            1,
            "daemon.isolated_sweeper.ttl_sweep_interval_ms",
        )?;
        require_absolute(&self.plugin.ppc_root, "daemon.plugin.ppc_root")?;
        require_u64_at_least(
            self.plugin.ppc_timeout_ms,
            1,
            "daemon.plugin.ppc_timeout_ms",
        )?;
        require_u64_at_least(
            self.plugin.service_probe_timeout_ms,
            1,
            "daemon.plugin.service_probe_timeout_ms",
        )?;
        require_usize_at_least(
            self.plugin.max_response_bytes,
            1,
            "daemon.plugin.max_response_bytes",
        )?;
        require_usize_at_least(
            self.layer_stack.auto_squash_max_depth,
            1,
            "daemon.layer_stack.auto_squash_max_depth",
        )?;
        require_usize_at_least(self.files.max_read_bytes, 1, "daemon.files.max_read_bytes")?;
        require_usize_at_least(
            self.files.max_write_bytes,
            1,
            "daemon.files.max_write_bytes",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn config_prd_daemon_section_deserializes_and_validates() {
        prd_config().validate().expect("prd daemon config is valid");
    }

    #[test]
    fn config_validation_rejects_invalid_daemon_values() {
        let mut cfg = prd_config();
        cfg.server.max_worker_threads = 0;
        assert_invalid(cfg, "daemon.server.max_worker_threads");

        let mut cfg = prd_config();
        cfg.inflight.ttl_s = 0.0;
        assert_invalid(cfg, "daemon.inflight.ttl_s");

        let mut cfg = prd_config();
        cfg.audit.pressure_threshold = 1.1;
        assert_invalid(cfg, "daemon.audit.pressure_threshold");

        let mut cfg = prd_config();
        cfg.command_sessions.cancel_wait_ms = 0;
        assert_invalid(cfg, "daemon.command_sessions.cancel_wait_ms");

        let mut cfg = prd_config();
        cfg.command_sessions.default_timeout_s = 0;
        assert_invalid(cfg, "daemon.command_sessions.default_timeout_s");

        let mut cfg = prd_config();
        cfg.plugin.ppc_root = PathBuf::from("relative");
        assert_invalid(cfg, "daemon.plugin.ppc_root");

        let mut cfg = prd_config();
        cfg.layer_stack.auto_squash_max_depth = 0;
        assert_invalid(cfg, "daemon.layer_stack.auto_squash_max_depth");

        let mut cfg = prd_config();
        cfg.files.max_write_bytes = 0;
        assert_invalid(cfg, "daemon.files.max_write_bytes");
    }

    #[test]
    fn config_plugin_child_module_does_not_own_config_rs() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let daemon_dir = manifest_dir
            .ancestors()
            .nth(2)
            .expect("eos-config lives below sandbox/crates")
            .join("crates/eos-daemon");
        assert!(
            !daemon_dir.join("src/services/plugins/config.rs").exists(),
            "plugin config must be owned by eos-config/src/configs/daemon.rs"
        );
    }

    fn prd_config() -> DaemonConfig {
        crate::load_prd()
            .expect("prd config loads")
            .section("daemon")
            .expect("daemon section deserializes")
    }

    fn assert_invalid(config: DaemonConfig, field: &str) {
        let err = config.validate().expect_err("config should be invalid");
        let message = err.to_string();
        assert!(message.contains(field), "{message}");
    }
}
