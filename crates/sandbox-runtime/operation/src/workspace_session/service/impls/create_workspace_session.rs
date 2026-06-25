use std::collections::hash_map::Entry;

use crate::timing;
use crate::workspace_crate::{CreateWorkspaceRequest, DestroyWorkspaceRequest};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{WorkspaceSession, WorkspaceSessionHandler};

impl WorkspaceSessionService {
    pub fn create_workspace_session(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        let total_started = std::time::Instant::now();
        let create_started = std::time::Instant::now();
        let handle = self.workspace().create_workspace(request)?;
        timing::duration(
            "operation.workspace_session.create_workspace",
            create_started,
        );
        let workspace_session_id = handle.id.clone();
        let cgroup_started = std::time::Instant::now();
        let cgroup_path = self.prepare_workspace_cgroup(&workspace_session_id);
        timing::duration("operation.workspace_session.prepare_cgroup", cgroup_started);
        let session = WorkspaceSession::from_handle(handle.clone(), cgroup_path.clone());
        let handler = session.handler();

        let insert_started = std::time::Instant::now();
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
        timing::duration("operation.workspace_session.insert_session", insert_started);

        if let Err(insert_error) = insert_result {
            let rollback_started = std::time::Instant::now();
            if let Err(rollback_error) = self
                .workspace()
                .destroy_workspace(handle, DestroyWorkspaceRequest::default())
            {
                timing::duration(
                    "operation.workspace_session.rollback_destroy",
                    rollback_started,
                );
                return Err(WorkspaceSessionError::CreateRollbackFailed {
                    workspace_session_id,
                    insert_error: Box::new(insert_error),
                    rollback_error,
                });
            }
            if let Some(cgroup_path) = &cgroup_path {
                let _ = std::fs::remove_dir(cgroup_path);
            }
            timing::duration(
                "operation.workspace_session.rollback_destroy",
                rollback_started,
            );
            return Err(insert_error);
        }

        timing::duration("operation.workspace_session.create_total", total_started);
        Ok(handler)
    }
}
