use std::collections::hash_map::Entry;

use sandbox_observability::record::names;
use serde_json::json;

use crate::workspace_crate::{CreateWorkspaceRequest, DestroyWorkspaceRequest};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{WorkspaceSession, WorkspaceSessionHandler};

impl WorkspaceSessionService {
    pub fn create_workspace_session(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.obs().scope(names::WORKSPACE_SESSION_CREATE, |_span| {
            let handle = self.workspace().create_workspace(request)?;
            let workspace_session_id = handle.id.clone();
            let cgroup_path = self.prepare_workspace_cgroup(&workspace_session_id);
            let session = WorkspaceSession::from_handle(handle.clone(), cgroup_path.clone());
            let handler = session.handler();

            let insert_result = self.lock_sessions().and_then(|mut sessions| {
                match sessions.entry(workspace_session_id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(session);
                        Ok(())
                    }
                    Entry::Occupied(_) => Err(WorkspaceSessionError::DuplicateWorkspaceSessionId {
                        workspace_session_id: workspace_session_id.clone(),
                    }),
                }
            });

            if let Err(insert_error) = insert_result {
                if let Err(rollback_error) = self
                    .workspace()
                    .destroy_workspace(handle, DestroyWorkspaceRequest::default())
                {
                    return Err(WorkspaceSessionError::CreateRollbackFailed {
                        workspace_session_id,
                        insert_error: Box::new(insert_error),
                        rollback_error,
                    });
                }
                if let Some(cgroup_path) = &cgroup_path {
                    let _ = std::fs::remove_dir(cgroup_path);
                }
                return Err(insert_error);
            }

            self.obs().event(
                names::LEASE_ACQUIRED,
                json!({ "revision": handler.handle.base_revision().version }),
            );
            Ok(handler)
        })
    }
}
