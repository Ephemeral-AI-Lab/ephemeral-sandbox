//! Command substrate DTOs.

use sandbox_runtime_namespace_execution::NamespaceExecutionTerminalStatus;

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
