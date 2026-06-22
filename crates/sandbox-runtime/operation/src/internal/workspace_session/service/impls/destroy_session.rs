use crate::workspace_crate::{DestroyWorkspaceRequest, DestroyWorkspaceResult};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::WorkspaceSessionHandler;

impl WorkspaceSessionService {
    pub fn destroy_session(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let mut sessions = self.lock_sessions()?;
        let session = sessions
            .get(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        let handle = session.active_handle()?;
        let cgroup_path = handle.entry().ok().and_then(|entry| entry.cgroup_path);
        let cgroup_final_sample = self
            .cgroup_monitor()
            .session_final_sample_from_handle(&handle);

        match self.workspace().destroy_workspace(handle, request) {
            Ok(result) => {
                self.cgroup_monitor().record_session_final_sample(
                    &handler.workspace_session_id,
                    cgroup_final_sample,
                );
                self.cgroup_monitor().record_cleanup(
                    &handler.workspace_session_id,
                    None,
                    cgroup_path.as_ref().map(|path| path.exists()),
                    None,
                );
                sessions.remove(&handler.workspace_session_id);
                Ok(result)
            }
            Err(error) => Err(WorkspaceSessionError::Workspace(error)),
        }
    }
}
