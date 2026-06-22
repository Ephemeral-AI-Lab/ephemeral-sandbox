//! Command substrate request DTOs and error type.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CommandError {
    /// A workspace-tier failure surfaced through the command lifecycle; the
    /// substrate carries only the rendered message.
    #[error("{0}")]
    Workspace(String),
    #[error("command not found: {0}")]
    NotFound(String),
    #[error("invalid command request: {0}")]
    InvalidRequest(String),
    #[error("command io error: {0}")]
    Io(String),
    #[error("command artifact write failed for {artifact} at {}: {error}", path.display())]
    ArtifactWrite {
        artifact: &'static str,
        path: PathBuf,
        error: String,
    },
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl CommandError {
    #[must_use]
    pub fn artifact_write(
        artifact: &'static str,
        path: impl AsRef<Path>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::ArtifactWrite {
            artifact,
            path: path.as_ref().to_path_buf(),
            error: error.to_string(),
        }
    }
}
