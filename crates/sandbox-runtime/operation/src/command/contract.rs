//! Command substrate types: the command configuration and the terminal result
//! projection the engine promise retains.

use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceExecutionTerminalStatus;
use sandbox_runtime_namespace_process::runner::protocol::CommandSecurityPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandConfig {
    pub scratch_root: PathBuf,
    pub command_security: CommandSecurityPolicy,
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            scratch_root: PathBuf::from("/eos/namespace_execution"),
            command_security: CommandSecurityPolicy::enforce(),
        }
    }
}

/// The trimmed terminal projection of a finished command: terminal status, exit
/// code, and total wall time. The command op's `finalize` builds it from a
/// `RunnerOutcome`; the engine promise retains it. `Copy` so the non-consuming
/// `resolved()` peek that serves terminal reads is trivial.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandTerminalResult {
    pub status: NamespaceExecutionTerminalStatus,
    pub exit_code: i64,
    pub command_total_time_seconds: f64,
}
