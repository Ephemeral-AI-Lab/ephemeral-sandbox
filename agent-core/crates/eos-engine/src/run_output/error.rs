/// Result alias for agent-run record operations.
pub type Result<T> = std::result::Result<T, AgentRunRecordError>;

/// File-backed agent-run record store failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentRunRecordError {
    /// A path segment would escape the record root or create ambiguous layout.
    #[error("unsafe agent-run record path segment for {field}: {value:?}")]
    UnsafeSegment {
        /// Field whose value was rejected.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// The requested agent-run record directory does not exist.
    #[error("agent-run record not found: {0}")]
    NotFound(String),
    /// A byte offset was beyond the current file length.
    #[error("message offset {offset} is beyond file length {len}")]
    OffsetOutOfRange {
        /// Requested offset.
        offset: u64,
        /// Current file length.
        len: u64,
    },
    /// Filesystem I/O failed.
    #[error("agent-run record io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding or decoding failed.
    #[error("agent-run record json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl AgentRunRecordError {
    pub(crate) fn missing_path(path: &std::path::Path) -> Self {
        Self::NotFound(path.display().to_string())
    }

    pub(crate) fn unsafe_segment(field: &'static str, value: impl Into<String>) -> Self {
        Self::UnsafeSegment {
            field,
            value: value.into(),
        }
    }
}
