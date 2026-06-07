use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionConfig {
    pub scratch_root: PathBuf,
    pub default_yield_time_ms: u64,
    pub default_timeout_s: u64,
    pub quiet_ms: u64,
    pub cancel_wait_ms: u64,
    pub output_drain_grace_ms: u64,
    pub max_session_s: u64,
    pub output_ring_max_bytes: usize,
    pub output_spool_max_bytes: u64,
}

impl Default for CommandSessionConfig {
    fn default() -> Self {
        Self {
            scratch_root: PathBuf::from("/eos/scratch/command-sessions"),
            default_yield_time_ms: 1000,
            default_timeout_s: 600,
            quiet_ms: 50,
            cancel_wait_ms: 500,
            output_drain_grace_ms: 500,
            max_session_s: 6 * 60 * 60,
            output_ring_max_bytes: 1024 * 1024,
            output_spool_max_bytes: 32 * 1024 * 1024,
        }
    }
}
