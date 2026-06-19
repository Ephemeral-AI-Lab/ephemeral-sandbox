use crate::error::WorkspaceError;
use crate::model::{CreateWorkspaceRequest, WorkspaceHandle, WorkspaceId};
use crate::profile::WorkspaceModeId;
use crate::service::support::{
    ensure_absolute, ensure_configured_workspace_root, ensure_non_empty,
    mode_snapshot_from_layerstack, workspace_error_from_mode_error,
};
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    pub fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        if let Some(hooks) = self.hooks() {
            return (hooks.create_workspace)(request);
        }

        ensure_non_empty(&request.caller_id.0, "caller_id")?;
        ensure_absolute(&request.workspace_root, "workspace_root")?;
        ensure_absolute(&request.layer_stack_root, "layer_stack_root")?;

        let mut state = self.lock_state()?;
        ensure_configured_workspace_root(&state.manager, &request.workspace_root)?;
        if let Some(existing_id) = state.manager.by_caller.get(&request.caller_id.0) {
            return Err(WorkspaceError::AlreadyOpen {
                owner: request.caller_id,
                workspace_id: WorkspaceId(existing_id.0.clone()),
            });
        }

        let snapshot = layerstack::service::acquire_snapshot_with_lease(
            &request.layer_stack_root,
            &request.caller_id.0,
        )
        .map_err(|error| WorkspaceError::SnapshotAcquire {
            source: error.to_string(),
        })?;
        let lease_id = snapshot.lease_id.clone();
        let mode_snapshot = mode_snapshot_from_layerstack(snapshot);
        let mode_handle = match state.manager.enter_with_profile(
            &request.caller_id.0,
            mode_snapshot,
            request.profile,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = layerstack::service::release_lease(&request.layer_stack_root, &lease_id);
                return Err(workspace_error_from_mode_error(
                    Some(&request.caller_id),
                    error,
                ));
            }
        };
        let mode_id = WorkspaceModeId(mode_handle.workspace_id.0.clone());
        state
            .layer_stack_roots
            .insert(mode_id, request.layer_stack_root);
        Ok(WorkspaceHandle::from(&mode_handle))
    }
}
