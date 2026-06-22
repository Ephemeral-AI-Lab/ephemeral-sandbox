use thiserror::Error;

use crate::workspace_crate::{WorkspaceError, WorkspaceSessionId};

#[derive(Debug, Error)]
pub enum WorkspaceSessionError {
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error("workspace session manager lock poisoned")]
    LockPoisoned,

    #[error("workspace session already exists: {workspace_session_id:?}")]
    DuplicateWorkspaceSessionId {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace session not found: {workspace_session_id:?}")]
    NotFound {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace remount already pending: {workspace_session_id:?}")]
    RemountAlreadyPending {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace remount blocked after failure: {workspace_session_id:?}")]
    RemountBlocked {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace remount is not pending: {workspace_session_id:?}")]
    RemountNotPending {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace remount returned mismatched workspace session id: expected {expected:?}, actual {actual:?}")]
    RemountWorkspaceSessionIdMismatch {
        expected: WorkspaceSessionId,
        actual: WorkspaceSessionId,
    },

    #[error(
        "workspace session publish captured changes failed for {workspace_session_id:?}: {error}"
    )]
    PublishCapturedChanges {
        workspace_session_id: WorkspaceSessionId,
        error: String,
    },

    #[error(
        "workspace cleanup after create failure failed for {workspace_session_id:?}: {rollback_error}"
    )]
    CreateRollbackFailed {
        workspace_session_id: WorkspaceSessionId,
        insert_error: Box<WorkspaceSessionError>,
        rollback_error: WorkspaceError,
    },
}

impl WorkspaceSessionError {
    pub(crate) fn not_found(workspace_session_id: &WorkspaceSessionId) -> Self {
        Self::NotFound {
            workspace_session_id: workspace_session_id.clone(),
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Workspace(error) => error.kind(),
            Self::LockPoisoned => "lock_poisoned",
            Self::DuplicateWorkspaceSessionId { .. } => "duplicate_workspace_session_id",
            Self::NotFound { .. } => "not_found",
            Self::RemountAlreadyPending { .. } => "remount_already_pending",
            Self::RemountBlocked { .. } => "remount_blocked",
            Self::RemountNotPending { .. } => "remount_not_pending",
            Self::RemountWorkspaceSessionIdMismatch { .. } => {
                "remount_workspace_session_id_mismatch"
            }
            Self::PublishCapturedChanges { .. } => "publish_captured_changes",
            Self::CreateRollbackFailed { .. } => "create_rollback_failed",
        }
    }
}
