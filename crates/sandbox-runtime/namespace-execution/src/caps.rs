use std::time::Duration;

use sandbox_runtime_namespace_process::runner::protocol::CommandSecurityProfile;

/// Execution-engine caps, injected by the operation layer from
/// `runtime.{command,namespace_execution}` config; `Default` preserves the
/// shipped policy. This crate never reads configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExecutionCaps {
    /// Concurrent live executions the registry admits.
    pub max_active: usize,
    /// Runner setup deadline in seconds.
    pub setup_timeout_s: f64,
    /// PTY stdin backpressure deadline.
    pub stdin_write_deadline: Duration,
    /// Terminal registry entries retained after completion.
    pub max_terminal_entries: usize,
    /// Byte window scanned from the transcript tail per read.
    pub max_transcript_window_bytes: u64,
    /// Runner result-pipe drain cap.
    pub max_runner_result_bytes: usize,
    /// Server-owned command-child capability and seccomp profile.
    pub command_security_profile: CommandSecurityProfile,
}

impl Default for ExecutionCaps {
    fn default() -> Self {
        Self {
            max_active: 32,
            setup_timeout_s: 30.0,
            stdin_write_deadline: Duration::from_secs(2),
            max_terminal_entries: 512,
            max_transcript_window_bytes: 1024 * 1024,
            max_runner_result_bytes: 8 * 1024 * 1024,
            command_security_profile: CommandSecurityProfile::Standard,
        }
    }
}
