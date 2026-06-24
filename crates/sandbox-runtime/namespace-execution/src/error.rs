use std::fmt;

/// Failures surfaced by the namespace execution engine.
#[derive(Debug, Clone)]
pub enum NamespaceExecutionError {
    /// The runner could not be launched (fork/pipe/PTY setup).
    Spawn(String),
    /// The runner launched but did not produce a usable completion envelope.
    Completion(String),
    /// A piped setup operation exceeded its setup deadline.
    Timeout { mode_flag: &'static str },
    /// An operation's `finalize` rejected the runner outcome.
    Finalize(String),
    /// Admission refused because `max_active` live executions are in flight.
    Admission { max_active: usize },
}

impl fmt::Display for NamespaceExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(detail) => write!(f, "failed to spawn namespace runner: {detail}"),
            Self::Completion(detail) => {
                write!(f, "namespace runner completion failed: {detail}")
            }
            Self::Timeout { mode_flag } => write!(f, "ns-runner {mode_flag} timed out"),
            Self::Finalize(detail) => {
                write!(f, "failed to finalize namespace execution: {detail}")
            }
            Self::Admission { max_active } => write!(
                f,
                "namespace execution admission refused: {max_active} active executions in flight"
            ),
        }
    }
}

impl std::error::Error for NamespaceExecutionError {}
