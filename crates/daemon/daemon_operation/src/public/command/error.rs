use std::path::PathBuf;

use thiserror::Error;

use crate::command::{CommandFinalizedMetadata, CommandSessionId};
use crate::workspace_crate::WorkspaceSessionId;

#[derive(Debug, Error)]
pub enum CommandServiceError {
    #[error(transparent)]
    WorkspaceSession(#[from] crate::workspace_session::WorkspaceSessionError),

    #[error("invalid command request: {message}")]
    InvalidCommand { message: String },

    #[error("command not found: {command_session_id:?}")]
    CommandNotFound {
        command_session_id: CommandSessionId,
    },

    #[error(
        "command workspace session mismatch for {command_session_id:?}: expected {expected:?}, actual {actual:?}"
    )]
    CommandWorkspaceSessionMismatch {
        command_session_id: CommandSessionId,
        expected: WorkspaceSessionId,
        actual: WorkspaceSessionId,
    },

    #[error("workspace session remount pending: {workspace_session_id:?}")]
    WorkspaceSessionRemountPending {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("command already completed: {command_session_id:?}")]
    CommandAlreadyCompleted {
        command_session_id: CommandSessionId,
    },

    #[error("command io failed for {command_session_id:?}: {error}")]
    CommandIo {
        command_session_id: CommandSessionId,
        error: String,
    },

    #[error("command transcript unavailable for {command_session_id:?} at {path:?}: {error}")]
    CommandTranscriptUnavailable {
        command_session_id: CommandSessionId,
        path: Option<PathBuf>,
        error: String,
    },

    #[error("command finalization failed for {command_session_id:?}: {error}")]
    CommandFinalizationFailed {
        command_session_id: CommandSessionId,
        error: String,
        finalized: Option<Box<CommandFinalizedMetadata>>,
    },

    #[error("duplicate command session id: {command_session_id:?}")]
    DuplicateCommandSessionId {
        command_session_id: CommandSessionId,
    },

    #[error("active command limit reached: active {active}, max {max}")]
    CommandAdmissionLimit { active: usize, max: usize },

    #[error("command reservation belongs to a different process store")]
    ReservationStoreMismatch,

    #[error(
        "command artifact cleanup failed for {command_session_id:?} after command start failure at {artifact_dir:?}: command error: {command_error}; cleanup error: {cleanup_error}"
    )]
    CommandArtifactCleanupFailed {
        command_session_id: CommandSessionId,
        command_error: Box<CommandServiceError>,
        artifact_dir: PathBuf,
        cleanup_error: String,
    },
}
