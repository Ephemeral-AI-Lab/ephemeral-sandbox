use thiserror::Error;

use sandbox_runtime_namespace_execution::NamespaceExecutionId;

use crate::workspace_crate::{WorkspaceError, WorkspaceSessionId};

use super::service::{PublishFailureStage, WorkspaceSessionPublishDetails};

#[derive(Debug, Clone, Error)]
pub enum WorkspaceSessionError {
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error("workspace session manager lock poisoned")]
    LockPoisoned,

    #[error("workspace session already exists: {workspace_session_id:?}")]
    DuplicateWorkspaceSessionId {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error(
        "raw workspace identity mismatch: reserved {reserved_workspace_session_id:?}, returned {returned_workspace_session_id:?}"
    )]
    WorkspaceIdentityMismatch {
        reserved_workspace_session_id: WorkspaceSessionId,
        returned_workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace session not found: {workspace_session_id:?}")]
    NotFound {
        workspace_session_id: WorkspaceSessionId,
    },

    #[error("workspace session has active command sessions: {workspace_session_id:?}")]
    ActiveCommands {
        workspace_session_id: WorkspaceSessionId,
        active_command_session_ids: Vec<NamespaceExecutionId>,
    },

    #[error(
        "workspace namespace holder exited for {workspace_session_id:?}: {reason} (cleanup state: {cleanup_state:?})"
    )]
    HolderExited {
        workspace_session_id: WorkspaceSessionId,
        reason: String,
        cleanup_state: super::service::FinalizationState,
    },

    #[error(
        "workspace session publish failed at {stage:?} for {workspace_session_id:?}: {diagnostic}"
    )]
    PublishRetained {
        workspace_session_id: WorkspaceSessionId,
        stage: PublishFailureStage,
        diagnostic: String,
        publish_rejection: Option<Box<sandbox_runtime_layerstack::PublishReject>>,
    },

    #[error(
        "workspace session published but could not be closed for {workspace_session_id:?}: {diagnostic}"
    )]
    PublishedButNotClosed {
        workspace_session_id: WorkspaceSessionId,
        publish: WorkspaceSessionPublishDetails,
        diagnostic: String,
    },

    #[error("workspace session finalization failed for {workspace_session_id:?}: {error}")]
    FinalizationFailed {
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

    #[error("workspace teardown remains incomplete for {workspace_session_id:?}: {failures:?}")]
    TeardownIncomplete {
        workspace_session_id: WorkspaceSessionId,
        failures: Vec<String>,
    },

    #[error(
        "workload cgroup setup failed for {workspace_session_id:?}: {diagnostic} (rollback: {rollback_diagnostic:?})"
    )]
    WorkloadCgroupSetupFailed {
        workspace_session_id: WorkspaceSessionId,
        diagnostic: String,
        rollback_diagnostic: Option<String>,
    },

    #[error("MPLA storage lifecycle rejected for {workspace_session_id:?}: {reason}")]
    MplaLifecycle {
        workspace_session_id: WorkspaceSessionId,
        reason: String,
    },
}

impl WorkspaceSessionError {
    pub(crate) fn not_found(workspace_session_id: &WorkspaceSessionId) -> Self {
        Self::NotFound {
            workspace_session_id: workspace_session_id.clone(),
        }
    }
}
