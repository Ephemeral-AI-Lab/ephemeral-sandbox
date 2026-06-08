use std::path::PathBuf;

use thiserror::Error;

/// Errors raised by fresh per-operation workspace policy.
#[derive(Debug, Error)]
pub enum EphemeralWorkspaceError {
    /// Fresh writable directory allocation failed.
    #[error("dir allocation failed at {}: {reason}", path.display())]
    DirAllocation { path: PathBuf, reason: String },
    /// Upperdir capture failed.
    #[error("capture failed: {reason}")]
    CaptureFailed { reason: String },
    /// Publishing captured changes failed.
    #[error("publish failed: {reason}")]
    PublishFailed { reason: String },
}
