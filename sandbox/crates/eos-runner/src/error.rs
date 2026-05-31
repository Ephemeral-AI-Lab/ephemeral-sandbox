//! Runner error type.
//!
//! Per the workspace non-negotiables: library errors are `thiserror` enums with
//! lowercase, punctuation-free messages and `#[from]` source conversions. The
//! kinds below mirror the active runner failure surfaces: invalid requests,
//! child process failures, overlay mount failures, timeouts, and the syscall
//! errnos raised by `setns` / `unshare`
//! (`isolated_workspace/scripts/_setns_libc.py:18-25`).

use thiserror::Error;

/// Failures returned by the namespace runner.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    /// A namespace syscall (`unshare`, `setns`, `mount`, `move_mount`) failed.
    /// Wraps the raw `errno`-bearing OS error.
    /// `// PORT backend/src/sandbox/isolated_workspace/scripts/_setns_libc.py:18-25`
    #[error("namespace syscall failed")]
    Syscall(#[source] std::io::Error),

    /// The request payload is structurally valid JSON but cannot be executed by
    /// this runner mode.
    #[error("invalid namespace runner request: {0}")]
    InvalidRequest(String),

    /// The overlay mount port failed.
    #[error("overlay mount failed")]
    Overlay(#[source] eos_overlay::OverlayError),

    /// Spawning, exec'ing, or waiting on the child process failed.
    /// `// PORT backend/src/sandbox/overlay/namespace_runner.py:243-272`
    #[error("child process failed")]
    Child(#[source] std::io::Error),

    /// The tool call exceeded its timeout; the group was SIGKILLed.
    /// `// PORT backend/src/sandbox/overlay/namespace_runner.py:265-269`
    #[error("tool call timed out")]
    TimedOut,

    /// Reached on a non-Linux host: the namespace syscalls do not exist. Lets
    /// the workspace compile and link on the macOS dev host (real runs are
    /// Linux/musl only).
    #[error("namespace runner is only supported on linux")]
    Unsupported,
}

impl From<std::io::Error> for RunnerError {
    fn from(err: std::io::Error) -> Self {
        Self::Syscall(err)
    }
}

impl From<eos_overlay::OverlayError> for RunnerError {
    fn from(err: eos_overlay::OverlayError) -> Self {
        Self::Overlay(err)
    }
}
