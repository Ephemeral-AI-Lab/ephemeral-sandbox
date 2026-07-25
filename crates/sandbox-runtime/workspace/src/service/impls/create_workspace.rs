use crate::error::WorkspaceError;
use crate::lifecycle::leases::next_handle_id;
use crate::model::{
    CreateWorkspaceRequest, LayerStackSnapshotRef, NetworkProfile, WorkspaceHandle,
    WorkspaceSessionId,
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
        let _admission = self.admit_work()?;
        if let Some(hooks) = self.hooks() {
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

        let candidate_admission = match state.storage_mode {
            WorkspaceStorageMode::Legacy => None,
            WorkspaceStorageMode::StrictCandidate {
                admission_lease_ttl,
                ..
            } => Some(
                sandbox_runtime_layerstack::service::acquire_hidden_candidate_generation(
                    &layer_stack_root,
                    "workspace-session",
                    &request.workspace_session_id.0,
                    admission_lease_ttl,
                )
                .map_err(|error| WorkspaceError::SnapshotAcquire {
                    source: format!("strict candidate exact admission failed: {error}"),
                })?,
            ),
        };
        let legacy_lease = match sandbox_runtime_layerstack::service::acquire_snapshot_with_lease(
            &layer_stack_root,
            "workspace-session",
        ) {
            Ok(lease) => lease,
            Err(error) => {
                let cleanup = candidate_admission
                        .as_ref()
                        .and_then(|admission| {
                            match sandbox_runtime_layerstack::service::release_candidate_generation_lease(
                                &layer_stack_root,
                                &admission.lease,
                            ) {
                                Ok(true) => None,
                                Ok(false) => Some(
                                    "candidate lease cleanup did not find the exact lease".to_owned(),
                                ),
                                Err(cleanup) => Some(format!(
                                    "candidate lease cleanup failed: {cleanup}"
                                )),
                            }
                        })
                        .map(|cleanup| format!("; {cleanup}"))
                        .unwrap_or_default();
                return Err(WorkspaceError::SnapshotAcquire {
                    source: format!("{error}{cleanup}"),
                });
            }
        };
        let mut snapshot = LayerStackSnapshotRef::from(legacy_lease);
        if let Some(admission) = &candidate_admission {
            snapshot.layer_paths = vec![admission.selection.carrier_path.clone()];
        }
        let strict_candidate_mount = candidate_admission.is_some();
        let mut session = match state.manager.open_with_candidate(
            request.workspace_session_id,
            snapshot,
            request.network,
            candidate_admission,
        ) {
            Ok(handle) => handle,
            Err(error) => return Err(workspace_error_from_manager_error(error)),
        };
        if strict_candidate_mount {
            if let Err(error) = sandbox_runtime_layerstack::service::record_hidden_candidate_mount(
                &layer_stack_root,
            ) {
                let cleanup = state
                    .manager
                    .close(&session.workspace_id, None)
                    .err()
                    .map(|cleanup| format!("; workspace rollback failed: {cleanup}"))
                    .unwrap_or_default();
                return Err(WorkspaceError::Setup {
                    step: format!("record strict candidate native mount failed: {error}{cleanup}"),
                });
            }
        }
        if let (
            WorkspaceStorageMode::StrictCandidate {
                session_lease_ttl, ..
            },
            Some(admission),
        ) = (state.storage_mode, session.candidate_admission.as_ref())
        {
            let renewed =
                match sandbox_runtime_layerstack::service::renew_candidate_generation_lease(
                    &layer_stack_root,
                    &admission.lease,
                    session_lease_ttl,
                ) {
                    Ok(lease) => lease,
                    Err(error) => {
                        let cleanup = state
                            .manager
                            .close(&session.workspace_id, None)
                            .err()
                            .map(|cleanup| format!("; workspace rollback failed: {cleanup}"))
                            .unwrap_or_default();
                        return Err(WorkspaceError::SnapshotAcquire {
                            source: format!(
                                "strict candidate session lease renewal failed: {error}{cleanup}"
                            ),
                        });
                    }
                };
            if let Err(error) = state
                .manager
                .replace_candidate_lease(&session.workspace_id, renewed)
            {
                let cleanup = state
                    .manager
                    .close(&session.workspace_id, None)
                    .err()
                    .map(|cleanup| format!("; workspace rollback failed: {cleanup}"))
                    .unwrap_or_default();
                return Err(WorkspaceError::Setup {
                    step: format!("persist renewed strict candidate lease: {error}{cleanup}"),
                });
            }
            session = state
                .manager
                .handle(&session.workspace_id)
                .cloned()
                .expect("renewed workspace remains registered");
        }
        state
            .manager
            .forget_completed_teardowns(&session.workspace_id);
        Ok(WorkspaceHandle::from(&session))
    }
}
