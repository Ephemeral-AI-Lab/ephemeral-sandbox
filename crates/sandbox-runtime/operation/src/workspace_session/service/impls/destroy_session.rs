use crate::timing;
use crate::workspace_crate::{DestroyWorkspaceRequest, DestroyWorkspaceResult};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::WorkspaceSessionHandler;

impl WorkspaceSessionService {
    pub fn destroy_session(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let total_started = std::time::Instant::now();
        let lock_started = std::time::Instant::now();
        let mut sessions = self.lock_sessions()?;
        timing::duration(
            "operation.workspace_session.destroy.lock_sessions",
            lock_started,
        );
        let session = sessions
            .get(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        let handle = session.active_handle()?;
        let cgroup_path = session.cgroup_path.clone();

        let destroy_started = std::time::Instant::now();
        match self.workspace().destroy_workspace(handle, request) {
            Ok(result) => {
                timing::duration(
                    "operation.workspace_session.destroy_workspace",
                    destroy_started,
                );
                let remove_started = std::time::Instant::now();
                sessions.remove(&handler.workspace_session_id);
                if let Some(cgroup_path) = &cgroup_path {
                    let _ = std::fs::remove_dir(cgroup_path);
                }
                timing::duration("operation.workspace_session.destroy.remove", remove_started);
                timing::duration("operation.workspace_session.destroy_total", total_started);
                Ok(result)
            }
            Err(error) => {
                timing::duration(
                    "operation.workspace_session.destroy_workspace",
                    destroy_started,
                );
                Err(WorkspaceSessionError::Workspace(error))
            }
        }
    }
}
