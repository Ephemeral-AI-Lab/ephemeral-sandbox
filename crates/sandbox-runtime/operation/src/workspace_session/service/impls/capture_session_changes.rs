use sandbox_observability::record::names;

use crate::workspace_crate::{CaptureChangesRequest, CapturedWorkspaceChanges};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::WorkspaceSessionHandler;

impl WorkspaceSessionService {
    pub fn capture_session_changes(
        &self,
        handler: &WorkspaceSessionHandler,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceSessionError> {
        self.obs()
            .scope(names::WORKSPACE_SESSION_CAPTURE_CHANGES, |_span| {
                let mut sessions = self.lock_sessions()?;
                let session = sessions
                    .get_mut(&handler.workspace_session_id)
                    .ok_or_else(|| {
                        WorkspaceSessionError::not_found(&handler.workspace_session_id)
                    })?;
                let handle = session.active_handle()?;
                let result = self.workspace().capture_changes(&handle, request)?;
                session.refresh_after_capture(result.base_revision.clone());

                Ok(result)
            })
    }
}
