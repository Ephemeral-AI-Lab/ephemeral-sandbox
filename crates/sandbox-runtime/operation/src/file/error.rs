use thiserror::Error;

use crate::layerstack::LayerStackServiceError;

/// Errors surfaced by the `file` domain. Blame's only failure is an unaudited
/// path; the owner string itself is opaque, so nothing here interprets it.
#[derive(Debug, Error)]
pub enum FileError {
    #[error("no auditability record for path: {0}")]
    NotFound(String),
}

/// File-type classification for a non-regular path. Regular files are read and
/// written; every other kind is rejected as an invalid request on both the
/// layerstack and namespace backends rather than followed or encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEntryKind {
    Directory,
    Symlink,
    Other,
}

/// Errors surfaced by the `read`/`write`/`edit` file operations. Peer to
/// [`FileError`]; the dispatch layer maps each variant to a `not_found`,
/// `invalid_request`, or `operation_failed` response kind.
#[derive(Debug, Error)]
pub enum FileOperationError {
    #[error("file not found: {0}")]
    NotFound(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("file is not valid UTF-8: {0}")]
    NotUtf8(String),
    #[error("path is not a regular file ({kind:?}): {path}")]
    NotRegular { path: String, kind: FileEntryKind },
    #[error("path is not a directory: {0}")]
    NotDirectory(String),
    #[error("list limit must be at least 1 (received {0})")]
    InvalidListLimit(usize),
    #[error("file is too large ({size} bytes; limit {limit}): {path}")]
    FileTooLarge {
        path: String,
        size: u64,
        limit: usize,
    },
    #[error("selected read output exceeds the maximum of {limit} bytes: {path}")]
    OutputTooLarge { path: String, limit: usize },
    #[error("string to replace not found in {path}: {snippet}")]
    EditNotFound { path: String, snippet: String },
    #[error("found {count} matches for edit in {path} but replace_all is false: {snippet}")]
    EditNotUnique {
        path: String,
        count: usize,
        snippet: String,
    },
    #[error("edits must not be empty")]
    NoEdits,
    #[error("edit made no changes to {0}")]
    NoChanges(String),
    #[error("workspace session not found: {0}")]
    WorkspaceSessionNotFound(String),
    #[error("workspace session file operation failed: {0}")]
    WorkspaceSession(String),
    #[error(transparent)]
    LayerStack(#[from] LayerStackServiceError),
    #[error("file i/o failed for {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}
