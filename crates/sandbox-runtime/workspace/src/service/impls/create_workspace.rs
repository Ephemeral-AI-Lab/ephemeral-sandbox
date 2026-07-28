use crate::error::WorkspaceError;
use crate::lifecycle::leases::next_handle_id;
use crate::model::{
    CreateWorkspaceRequest, ExternalOverlayLayout, LayerStackSnapshotRef, NetworkProfile,
    WorkspaceHandle, WorkspaceSessionId,
};
use crate::service::support::{ensure_absolute, workspace_error_from_manager_error};
use crate::service::{WorkspaceRuntimeService, WorkspaceStorageMode};

impl WorkspaceRuntimeService {
    /// Allocate the identity that the operation layer reserves before any raw
    /// workspace or cgroup resource is created.
    pub fn allocate_workspace_session_id(
        &self,
        network: NetworkProfile,
    ) -> Result<WorkspaceSessionId, WorkspaceError> {
        let _admission = self.admit_work()?;
        if let Some(hooks) = self.hooks() {
            return (hooks.allocate_workspace_session_id)(network);
        }
        Ok(WorkspaceSessionId(next_handle_id()))
    }

    pub fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        self.create_workspace_with_optional_external_overlay(request, None)
    }

    /// Create a holder for a server-prepared MPLA overlay.  It deliberately
    /// skips mounting; only the typed storage-admin helper may mount it.
    pub fn create_workspace_with_external_overlay(
        &self,
        request: CreateWorkspaceRequest,
        external_overlay: ExternalOverlayLayout,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        self.create_workspace_with_optional_external_overlay(request, Some(external_overlay))
    }

    fn create_workspace_with_optional_external_overlay(
        &self,
        request: CreateWorkspaceRequest,
        external_overlay: Option<ExternalOverlayLayout>,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        let _admission = self.admit_work()?;
        if let Some(hooks) = self.hooks() {
            if external_overlay.is_some() {
                return Err(WorkspaceError::Setup {
                    step: "workspace runtime hooks do not implement external MPLA overlays"
                        .to_owned(),
                });
            }
            return (hooks.create_workspace)(request);
        }

        let _ = self.reconcile_pending_teardowns();
        let mut state = self.lock_state()?;
        let layer_stack_root = state.layer_stack_root.clone();
        ensure_absolute(&layer_stack_root, "layer_stack_root")?;
        state
            .manager
            .ensure_workspace_available(&request.workspace_session_id)
            .map_err(workspace_error_from_manager_error)?;

        let (candidate_admission, candidate_session_lease_ttl, legacy_lease) = match state
            .storage_mode
        {
            WorkspaceStorageMode::Legacy => (
                None,
                None,
                sandbox_runtime_layerstack::service::acquire_snapshot_with_lease(
                    &layer_stack_root,
                    "workspace-session",
                )
                .map_err(|error| WorkspaceError::SnapshotAcquire {
                    source: error.to_string(),
                })?,
            ),
            WorkspaceStorageMode::StrictCandidate {
                admission_lease_ttl,
                session_lease_ttl,
            } => {
                let (admission, snapshot) =
                    sandbox_runtime_layerstack::service::acquire_hidden_candidate_generation_with_snapshot(
                        &layer_stack_root,
                        "workspace-session",
                        &request.workspace_session_id.0,
                        admission_lease_ttl,
                    )
                    .map_err(|error| WorkspaceError::SnapshotAcquire {
                        source: format!("strict candidate exact admission failed: {error}"),
                    })?;
                (Some(admission), Some(session_lease_ttl), snapshot)
            }
        };
        let mut snapshot = LayerStackSnapshotRef::from(legacy_lease);
        if let Some(admission) = &candidate_admission {
            snapshot.layer_paths = vec![admission.selection.carrier_path.clone()];
        }
        let session = match state.manager.open_with_candidate_with_external_overlay(
            request.workspace_session_id,
            snapshot,
            request.network,
            candidate_admission,
            candidate_session_lease_ttl,
            external_overlay,
        ) {
            Ok(handle) => handle,
            Err(error) => return Err(workspace_error_from_manager_error(error)),
        };
        state
            .manager
            .forget_completed_teardowns(&session.workspace_id);
        Ok(WorkspaceHandle::from(&session))
    }
}
