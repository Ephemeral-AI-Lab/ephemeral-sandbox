use std::sync::{OnceLock, RwLock};

#[cfg(target_os = "linux")]
use eos_command_session::CommandSessionConfig as RuntimeCommandSessionConfig;

use crate::config::CommandSessionConfig;

pub(crate) fn configure_command_sessions(config: &CommandSessionConfig) {
    let mut guard = command_session_config_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = config.clone();
}

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn command_session_config() -> CommandSessionConfig {
    command_session_config_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(target_os = "linux")]
pub(super) fn runtime_command_session_config() -> RuntimeCommandSessionConfig {
    let config = command_session_config();
    RuntimeCommandSessionConfig {
        scratch_root: config.scratch_root,
        default_yield_time_ms: config.default_yield_time_ms,
        quiet_ms: config.quiet_ms,
        cancel_wait_ms: config.cancel_wait_ms,
        output_drain_grace_ms: config.output_drain_grace_ms,
        max_session_s: config.max_session_s,
        output_ring_max_bytes: config.output_ring_max_bytes,
        output_spool_max_bytes: config.output_spool_max_bytes,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn command_session_scratch_root() -> std::path::PathBuf {
    command_session_config().scratch_root
}

fn command_session_config_cell() -> &'static RwLock<CommandSessionConfig> {
    static CONFIG: OnceLock<RwLock<CommandSessionConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(default_command_session_config()))
}

fn default_command_session_config() -> CommandSessionConfig {
    CommandSessionConfig {
        scratch_root: std::path::PathBuf::from("/eos/scratch/command-sessions"),
        default_yield_time_ms: 1000,
        quiet_ms: 50,
        cancel_wait_ms: 500,
        output_drain_grace_ms: 500,
        max_session_s: 6 * 60 * 60,
        output_ring_max_bytes: 1024 * 1024,
        output_spool_max_bytes: 32 * 1024 * 1024,
    }
}
