/// Result alias for message-record operations.
pub type Result<T> = std::result::Result<T, MessageRecordError>;

/// File-backed message-record service failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MessageRecordError {
    /// A path segment would escape the message-record root or create ambiguous layout.
    #[error("unsafe message-record path segment for {field}: {value:?}")]
    UnsafeSegment {
        /// Field whose value was rejected.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// The requested agent-run message-record directory does not exist.
    #[error("agent-run message record not found: {0}")]
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
    #[error("message-record io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding or decoding failed.
    #[error("message-record json error: {0}")]
    Json(#[from] serde_json::Error),
    /// A blocking filesystem scan panicked or was cancelled.
    #[error("message-record scan task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl MessageRecordError {
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
