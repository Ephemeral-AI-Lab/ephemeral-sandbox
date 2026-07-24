//! Command substrate types: the command configuration and the terminal result
//! projection the engine promise retains.

use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceExecutionTerminalStatus;

use crate::services::{CommandRuntimeConfig, NamespaceExecutionCaps};

#[derive(Debug, Clone, PartialEq)]
pub struct CommandConfig {
    pub scratch_root: PathBuf,
    pub max_active: usize,
    pub setup_timeout_s: f64,
    pub read_lines_default: usize,
    pub read_lines_max: usize,
    pub execution: NamespaceExecutionCaps,
}

impl Default for CommandConfig {
    fn default() -> Self {
        let command = CommandRuntimeConfig::default();
        Self {
            scratch_root: PathBuf::from("/eos/workspace"),
            max_active: command.max_active,
            setup_timeout_s: 30.0,
            read_lines_default: command.read_lines_default,
            read_lines_max: command.read_lines_max,
            execution: NamespaceExecutionCaps::default(),
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
