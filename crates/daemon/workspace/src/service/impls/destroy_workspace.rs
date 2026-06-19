use crate::error::WorkspaceError;
use crate::model::{DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceHandle};
use crate::service::support::{active_mode_id, workspace_error_from_mode_error};
use crate::service::WorkspaceRuntimeService;

impl WorkspaceRuntimeService {
    pub fn destroy_workspace(
        &self,
        handle: WorkspaceHandle,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceError> {
        if let Some(hooks) = self.hooks() {
            return (hooks.destroy_workspace)(handle, request);
        }

        let (layer_stack_root, outcome) = {
            let mut state = self.lock_state()?;
            let mode_id = active_mode_id(&state, &handle)?;
            let layer_stack_root =
                state
                    .layer_stack_roots
                    .remove(&mode_id)
                    .ok_or_else(|| WorkspaceError::Setup {
                        step: format!("missing layer stack root for workspace {}", handle.id.0),
                    })?;
            let outcome = match state.manager.exit(&handle.owner.0, request.grace_s) {
                Ok(outcome) => outcome,
                Err(error) => {
                    state.layer_stack_roots.insert(mode_id, layer_stack_root);
                    return Err(workspace_error_from_mode_error(Some(&handle.owner), error));
                }
            };
            (layer_stack_root, outcome)
        };

        let release = layerstack::service::release_lease(&layer_stack_root, &outcome.lease_id);
        let (lease_released, mut lease_release_error) = match release {
            Ok(()) => (Some(true), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let active_leases_after = match layerstack::LayerStack::open(layer_stack_root) {
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

        Ok(DestroyWorkspaceResult {
            workspace_id: handle.id,
            owner: handle.owner,
            evicted_upperdir_bytes: outcome.evicted_upperdir_bytes,
            lifetime_s: outcome.lifetime_s,
            lease_released,
            lease_release_error,
            active_leases_after,
        })
    }
}
