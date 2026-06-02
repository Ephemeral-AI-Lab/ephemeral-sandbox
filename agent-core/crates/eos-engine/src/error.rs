//! Engine error type.

use eos_llm_client::ProviderError;
use eos_tools::ToolError;
use eos_types::CoreError;

/// A framework error raised by the engine loop or one of its owned helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// Provider/client error.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Tool framework error.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// Shared value/store error.
    #[error("core error: {0}")]
    Core(#[from] CoreError),

    /// Prompt-report file I/O error.
    #[error("prompt report io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A model requested a tool that is not registered.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// Query loop was run without an event source.
    #[error("query context has no event source")]
    MissingEventSource,

    /// Engine invariant broke.
    #[error("internal engine error: {0}")]
    Internal(String),
}
