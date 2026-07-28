use std::collections::hash_map::Entry;

use sandbox_runtime_mpla_poc::allocation::{create_allocation, destroy_workspace_allocation};
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::{prepare_external_session, OperationId, RunId, SessionId};
use serde_json::json;

use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, ExternalOverlayLayout, NetworkProfile,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::cgroup::cleanup_workspace_cgroup;
use super::super::model::{MplaStoragePhase, MplaWorkspaceBinding, WorkspaceSession};

impl WorkspaceSessionService {
    /// Create the only kind of workspace session whose overlay is owned by the
    /// storage-admin helper.  The holder is created with an empty mountpoint;
    /// no ordinary workload can use it before a successful `Mount` receipt.
    pub fn create_mpla_workspace_session(
        &self,
        run_id: RunId,
        operation_id: OperationId,
    ) -> Result<super::super::model::WorkspaceSessionHandler, WorkspaceSessionError> {
        self.obs().scope("mpla_workspace_session_create", |span| {
            span.attr("run_id", run_id.as_str().to_owned());
            let roots = self.mpla_lifecycle_roots.clone().ok_or_else(|| {
                WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: crate::workspace_crate::WorkspaceSessionId(
                        "unallocated".to_owned(),
                    ),
                    reason: "MPLA lifecycle roots are not configured".to_owned(),
                }
            })?;
            let workspace_session_id = self
                .workspace()
                .allocate_workspace_session_id(NetworkProfile::Shared)?;
            let _reservation = self.reserve_workspace_session_id(workspace_session_id.clone())?;

            let allocations_root = roots.payload_root.join("allocations");
            let allocation = create_allocation(&allocations_root, &operation_id)
                .map_err(|error| mpla_error(&workspace_session_id, "create allocation", error))?;
            let lease = match issue_workspace_lease(&allocation, SessionId::new(), &operation_id) {
                Ok(lease) => lease,
                Err(error) => {
                    return Err(mpla_error(
                        &workspace_session_id,
                        "issue workspace lease",
                        error,
                    ));
                }
            };
            let prepared = match prepare_external_session(&roots.control_root, &allocation, &lease)
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(mpla_error(
                        &workspace_session_id,
                        "prepare external session",
                        error,
                    ));
                }
            };
            let external_overlay = ExternalOverlayLayout {
                workspace_root: prepared.workspace_root().to_path_buf(),
                upperdir: allocation.upper_dir.clone(),
                workdir: allocation.work_dir.clone(),
            };
            let handle = match self.workspace().create_workspace_with_external_overlay(
                CreateWorkspaceRequest {
                    workspace_session_id: workspace_session_id.clone(),
                    network: NetworkProfile::Shared,
                },
                external_overlay,
            ) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(error.into());
                }
            };
            if handle.id != workspace_session_id {
                let returned_workspace_session_id = handle.id.clone();
                let _ = self
                    .workspace()
                    .destroy_workspace(handle, DestroyWorkspaceRequest::default());
                let _ = destroy_workspace_allocation(
                    &allocations_root,
                    &allocation.descriptor.allocation_id,
                    &lease.deleter,
                );
                return Err(WorkspaceSessionError::WorkspaceIdentityMismatch {
                    reserved_workspace_session_id: workspace_session_id,
                    returned_workspace_session_id,
                });
            }
            let cgroup_path = match self.prepare_workspace_cgroup(&workspace_session_id) {
                Ok(path) => path,
                Err(error) => {
                    let _ = self
                        .workspace()
                        .destroy_workspace(handle.clone(), DestroyWorkspaceRequest::default());
                    self.workspace().commit_workspace_destroy(&handle);
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(error);
                }
            };
            let binding = MplaWorkspaceBinding {
                run_id,
                payload_root: roots.payload_root,
                control_root: roots.control_root,
                storage_admin_profile: roots.storage_admin_profile,
                allocation,
                lease,
                lease_operation_id: operation_id,
                prepared,
                mount_scope: None,
                mount_receipt_binding: None,
                cleanup_operation_id: None,
                phase: MplaStoragePhase::Prepared,
            };
            let session =
                WorkspaceSession::from_mpla_handle(handle.clone(), cgroup_path.clone(), binding);
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
            if let Err(error) = insert_result {
                if let Err(rollback_error) = self
                    .workspace()
                    .destroy_workspace(handle.clone(), DestroyWorkspaceRequest::default())
                {
                    return Err(WorkspaceSessionError::CreateRollbackFailed {
                        workspace_session_id,
                        insert_error: Box::new(error),
                        rollback_error,
                    });
                }
                self.workspace().commit_workspace_destroy(&handle);
                if let Some(cgroup_path) = &cgroup_path {
                    let _ = cleanup_workspace_cgroup(cgroup_path);
                }
                return Err(error);
            }
            self.obs().event(
                sandbox_observability_telemetry::record::names::LEASE_ACQUIRED,
                json!({ "revision": handler.handle.base_revision().version, "mpla": true }),
            );
            self.commit_created_session(&handler)?;
            Ok(handler)
        })
    }
}

fn mpla_error(
    workspace_session_id: &crate::workspace_crate::WorkspaceSessionId,
    action: &str,
    error: impl std::fmt::Display,
) -> WorkspaceSessionError {
    WorkspaceSessionError::MplaLifecycle {
        workspace_session_id: workspace_session_id.clone(),
        reason: format!("{action}: {error}"),
    }
}
