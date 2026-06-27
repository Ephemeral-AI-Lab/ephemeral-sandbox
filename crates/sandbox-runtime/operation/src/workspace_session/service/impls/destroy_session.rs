use sandbox_observability::record::names;
use serde_json::json;

use crate::workspace_crate::{DestroyWorkspaceRequest, DestroyWorkspaceResult};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::WorkspaceSessionHandler;

impl WorkspaceSessionService {
    pub fn destroy_session(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        self.obs().scope(names::WORKSPACE_SESSION_DESTROY, |_span| {
            let mut sessions = self.lock_sessions()?;
            let session = sessions
                .get(&handler.workspace_session_id)
                .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
            let handle = session.active_handle()?;
            let cgroup_path = session.cgroup_path.clone();
            let revision = handle.base_revision().version;

            match self.workspace().destroy_workspace(handle, request) {
                Ok(result) => {
                    sessions.remove(&handler.workspace_session_id);
                    if let Some(cgroup_path) = &cgroup_path {
                        let _ = std::fs::remove_dir(cgroup_path);
                    }
                    self.obs()
                        .event(names::LEASE_RELEASED, json!({ "revision": revision }));
                    Ok(result)
                }
                Err(error) => Err(WorkspaceSessionError::Workspace(error)),
            }
        })
    }
}
