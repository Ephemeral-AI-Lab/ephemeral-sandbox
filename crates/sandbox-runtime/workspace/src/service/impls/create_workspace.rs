use crate::error::WorkspaceError;
use crate::model::{CreateWorkspaceRequest, WorkspaceHandle};
use crate::service::support::{
    ensure_absolute, mode_snapshot_from_layerstack, workspace_error_from_mode_error,
};
use crate::service::WorkspaceRuntimeService;
use crate::timing;

impl WorkspaceRuntimeService {
    pub fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        let total_started = std::time::Instant::now();
        if let Some(hooks) = self.hooks() {
            let result = (hooks.create_workspace)(request);
            timing::duration("workspace.create.total", total_started);
            return result;
        }

        let lock_started = std::time::Instant::now();
        let mut state = self.lock_state()?;
        timing::duration("workspace.create.lock_state", lock_started);
        let layer_stack_root = state.layer_stack_root.clone();
        ensure_absolute(&layer_stack_root, "layer_stack_root")?;

        let snapshot_started = std::time::Instant::now();
        let snapshot = sandbox_runtime_layerstack::service::acquire_snapshot_with_lease(
            &layer_stack_root,
            "workspace-session",
        )
        .map_err(|error| WorkspaceError::SnapshotAcquire {
            source: error.to_string(),
        })?;
        timing::duration(
            "workspace.create.layerstack.acquire_snapshot",
            snapshot_started,
        );
        let lease_id = snapshot.lease_id.clone();
        let mode_snapshot = mode_snapshot_from_layerstack(snapshot);
        let enter_started = std::time::Instant::now();
        let mode_handle = match state
            .manager
            .enter_with_profile(mode_snapshot, request.profile)
        {
            Ok(handle) => {
                timing::duration("workspace.create.profile.enter", enter_started);
                handle
            }
            Err(error) => {
                timing::duration("workspace.create.profile.enter", enter_started);
                let release_started = std::time::Instant::now();
                let _ = sandbox_runtime_layerstack::service::release_lease(
                    &layer_stack_root,
                    &lease_id,
                );
                timing::duration("workspace.create.layerstack.release_lease", release_started);
                return Err(workspace_error_from_mode_error(error));
            }
        };
        timing::duration("workspace.create.total", total_started);
        Ok(WorkspaceHandle::from(&mode_handle))
    }
}
