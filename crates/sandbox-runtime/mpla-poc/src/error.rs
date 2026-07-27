use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PocError {
    #[error("invalid run id: {0}")]
    InvalidRunId(String),
    #[error("invalid fixed configuration: {0}")]
    InvalidConfig(String),
    #[error("I/O failure during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("clock failure: {0}")]
    Clock(String),
    #[error("unsupported qualification profile: {0}")]
    Unsupported(String),
    #[error("integrity failure: {0}")]
    Integrity(String),
    #[error(
        "stale {capability} capability for allocation {allocation_id}: expected epoch {expected_epoch}, observed {observed_epoch}"
    )]
    StaleCapability {
        capability: &'static str,
        allocation_id: String,
        expected_epoch: u64,
        observed_epoch: u64,
    },
    #[error("owner compare-and-adopt conflict: {0}")]
    OwnerConflict(String),
    #[error("durable state is recovery-required: {0}")]
    RecoveryRequired(String),
    #[error("overlay failure: {0}")]
    Overlay(#[from] sandbox_runtime_overlay::OverlayError),
}

impl PocError {
    pub fn io(operation: &'static str, path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

pub type PocResult<T> = Result<T, PocError>;
