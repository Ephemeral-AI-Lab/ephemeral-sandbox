use crate::error::WorkspaceError;
use crate::model::{DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceHandle};
use crate::service::support::{active_mode_id, workspace_error_from_mode_error};
use crate::service::WorkspaceRuntimeService;
use crate::timing;

impl WorkspaceRuntimeService {
    pub fn destroy_workspace(
        &self,
        handle: WorkspaceHandle,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceError> {
        let total_started = std::time::Instant::now();
        if let Some(hooks) = self.hooks() {
            let result = (hooks.destroy_workspace)(handle, request);
            timing::duration("workspace.destroy.total", total_started);
            return result;
        }

        let (layer_stack_root, outcome) = {
            let lock_started = std::time::Instant::now();
            let mut state = self.lock_state()?;
            timing::duration("workspace.destroy.lock_state", lock_started);
            let mode_id = active_mode_id(&state, &handle)?;
            let layer_stack_root = state.layer_stack_root.clone();
            let exit_started = std::time::Instant::now();
            let outcome = match state.manager.exit(&mode_id, request.grace_s) {
                Ok(outcome) => {
                    timing::duration("workspace.destroy.profile.exit", exit_started);
                    outcome
                }
                Err(error) => {
                    timing::duration("workspace.destroy.profile.exit", exit_started);
                    return Err(workspace_error_from_mode_error(error));
                }
            };
            (layer_stack_root, outcome)
        };

        let release_started = std::time::Instant::now();
        let release = sandbox_runtime_layerstack::service::release_lease(
            &layer_stack_root,
            &outcome.lease_id,
        );
        timing::duration(
            "workspace.destroy.layerstack.release_lease",
            release_started,
        );
        let (lease_released, mut lease_release_error) = match release {
            Ok(()) => (Some(true), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let count_started = std::time::Instant::now();
        let active_leases_after =
            match sandbox_runtime_layerstack::LayerStack::open(layer_stack_root) {
                Ok(stack) => stack.active_lease_count(),
                Err(error) => {
                    let message = format!("count active leases after destroy: {error}");
                    if let Some(existing) = lease_release_error.as_mut() {
                        existing.push_str("; ");
                        existing.push_str(&message);
                    } else {
                        lease_release_error = Some(message);
                    }
                    0
                }
            };
        timing::duration(
            "workspace.destroy.layerstack.count_active_leases",
            count_started,
        );

        let result = DestroyWorkspaceResult {
            workspace_session_id: handle.id,
            evicted_upperdir_bytes: outcome.evicted_upperdir_bytes,
            lifetime_s: outcome.lifetime_s,
            lease_released,
            lease_release_error,
            active_leases_after,
        };
        timing::duration("workspace.destroy.total", total_started);
        Ok(result)
    }
}
