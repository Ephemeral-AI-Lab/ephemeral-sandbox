use std::collections::hash_map::Entry;

use crate::workspace_crate::{CreateWorkspaceRequest, DestroyWorkspaceRequest};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{WorkspaceSession, WorkspaceSessionHandler};
use tracing::{field, Span};

impl WorkspaceSessionService {
    pub fn create_workspace_session(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        let span = tracing::info_span!(
            "workspace.create_session",
            profile = request.profile.as_str(),
            status = field::Empty,
            error_kind = field::Empty,
        );
        let _span_guard = span.enter();
        let result = self.create_workspace_session_inner(request);
        record_create_session_result(&span, &result);
        result
    }

    fn create_workspace_session_inner(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        let handle = self.workspace().create_workspace(request)?;
        let workspace_session_id = handle.id.clone();
        let session = WorkspaceSession::from_handle(handle.clone());
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
            return Err(insert_error);
        }

        self.cgroup_monitor().register_session_from_handle(&handle);

        Ok(handler)
    }
}

fn record_create_session_result(
    span: &Span,
    result: &Result<WorkspaceSessionHandler, WorkspaceSessionError>,
) {
    match result {
        Ok(_) => {
            span.record("status", "ok");
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
        }
    }
}
