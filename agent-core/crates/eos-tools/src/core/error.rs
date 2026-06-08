//! Tool framework error type.

use eos_sandbox_port::SandboxPortError;
use eos_types::CoreError;

/// A framework fault during tool execution. Tool-domain failures are in-band
/// [`crate::ToolResult`]s, not variants here.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// The dispatched tool name is not registered.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// A required execution-context value was absent where the tool requires it.
    #[error("missing required execution context: {0}")]
    MissingContext(&'static str),

    /// A required downstream-state port was not wired at the composition root.
    #[error("required port not wired: {0}")]
    MissingPort(&'static str),

    /// An upstream `Store` operation failed.
    #[error("store error: {0}")]
    Store(#[from] CoreError),

    /// A sandbox transport / daemon RPC failed at the framework level.
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxPortError),

    /// An internal invariant broke.
    #[error("internal tool error: {0}")]
    Internal(String),
}
