use crate::workspace_crate::WorkspaceSessionId;

pub use sandbox_runtime_namespace_execution::{
    NamespaceExecutionId, NamespaceExecutionTerminalStatus,
};

/// Move-only evidence that command ownership reached zero for one workspace.
///
/// Construction is private to the command teardown owner and happens only
/// after terminal leases have been evicted and no active lease remains.
#[derive(Debug)]
#[must_use = "workspace storage must not be deleted without consuming the release proof"]
pub(crate) struct WorkspaceCommandReleaseProof {
    workspace_session_id: WorkspaceSessionId,
    released_terminal_records: usize,
}

impl WorkspaceCommandReleaseProof {
    pub(crate) fn new(
        workspace_session_id: WorkspaceSessionId,
        released_terminal_records: usize,
    ) -> Self {
        Self {
            workspace_session_id,
            released_terminal_records,
        }
    }

    pub(crate) fn verifies(&self, workspace_session_id: &WorkspaceSessionId) -> bool {
        self.workspace_session_id == *workspace_session_id
    }

    pub(crate) const fn released_terminal_records(&self) -> usize {
        self.released_terminal_records
    }
}

/// Narrow ownership port used by workspace teardown. The workspace service
/// knows only which admitted command ids must be drained; command execution
/// owns the concrete engine handles, cancellation, and bounded joins.
pub(crate) trait WorkspaceCommandTeardown: Send + Sync {
    fn cancel_and_join(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_ids: &[NamespaceExecutionId],
    ) -> Result<(), String>;

    fn release_for_destroy(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<WorkspaceCommandReleaseProof, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeNamespaceExecutionSnapshot {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub command: Option<String>,
}
