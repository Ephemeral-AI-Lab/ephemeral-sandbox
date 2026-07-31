use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sandbox_runtime_mpla_poc::allocation::{create_allocation, destroy_workspace_allocation};
use sandbox_runtime_mpla_poc::durable::begin_durability_batch;
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::{
    inherit_projection_root_metadata, prepare_external_session, OperationId, PairedRefValue, RunId,
    SessionId,
};
use serde_json::json;

use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, ExternalOverlayLayout, NetworkProfile,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::cgroup::cleanup_workspace_cgroup;
use super::super::model::{MplaStoragePhase, MplaWorkspaceBinding, WorkspaceSession};

#[derive(Debug, Default)]
pub(crate) struct MplaWorkspaceCreateTimings {
    pub(crate) session_identity_elapsed_ns: u64,
    pub(crate) allocation_create_elapsed_ns: u64,
    pub(crate) allocation_lease_elapsed_ns: u64,
    pub(crate) projection_metadata_elapsed_ns: u64,
    pub(crate) external_session_prepare_elapsed_ns: u64,
    pub(crate) durability_commit_elapsed_ns: u64,
    pub(crate) workspace_create_elapsed_ns: u64,
    pub(crate) launch_material_elapsed_ns: u64,
    pub(crate) cgroup_prepare_elapsed_ns: u64,
    pub(crate) session_register_elapsed_ns: u64,
    pub(crate) session_commit_elapsed_ns: u64,
}

impl WorkspaceSessionService {
    /// Create the only kind of workspace session whose overlay is owned by the
    /// storage-admin helper.  The holder is created with an empty mountpoint;
    /// no ordinary workload can use it before a successful `Mount` receipt.
    pub fn create_mpla_workspace_session(
        &self,
        run_id: RunId,
        operation_id: OperationId,
    ) -> Result<super::super::model::WorkspaceSessionHandler, WorkspaceSessionError> {
        self.create_mpla_workspace_session_with_projection(run_id, operation_id, None, true)
            .map(|(handler, _)| handler)
    }

    pub(crate) fn create_mpla_workspace_session_with_projection(
        &self,
        run_id: RunId,
        operation_id: OperationId,
        projection: Option<(PairedRefValue, Vec<std::path::PathBuf>)>,
        commit_object_graph: bool,
    ) -> Result<
        (
            super::super::model::WorkspaceSessionHandler,
            MplaWorkspaceCreateTimings,
        ),
        WorkspaceSessionError,
    > {
        self.obs().scope("mpla_workspace_session_create", |span| {
            span.attr("run_id", run_id.as_str().to_owned());
            let mut timings = MplaWorkspaceCreateTimings::default();
            let session_identity_started = Instant::now();
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
            timings.session_identity_elapsed_ns = elapsed_ns(session_identity_started);

            // The fresh random allocation and session are not discoverable
            // until the workspace holder is registered below. Defer their
            // individual barriers and commit only the exact touched object
            // graph before any authority is published.
            let durability_batch = begin_durability_batch();
            let allocations_root = roots.payload_root.join("allocations");
            let allocation_create_started = Instant::now();
            let allocation = create_allocation(&allocations_root, &operation_id)
                .map_err(|error| mpla_error(&workspace_session_id, "create allocation", error))?;
            timings.allocation_create_elapsed_ns = elapsed_ns(allocation_create_started);
            let allocation_lease_started = Instant::now();
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
            timings.allocation_lease_elapsed_ns = elapsed_ns(allocation_lease_started);
            let projection_metadata_started = Instant::now();
            let requested_lower_dirs_newest_first = match requested_mpla_lower_dirs(
                projection
                    .as_ref()
                    .map(|(_, lower_dirs)| lower_dirs.as_slice()),
                &allocation.allocation_root,
            ) {
                Ok(lower_dirs) => lower_dirs,
                Err(error) => {
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(mpla_error(
                        &workspace_session_id,
                        "create initial empty MPLA lower directory",
                        error,
                    ));
                }
            };
            if let Some((_, lower_dirs)) = &projection {
                let Some(selected_root) = lower_dirs.first() else {
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(mpla_error(
                        &workspace_session_id,
                        "inherit projection root metadata",
                        "exact projection selected no payload root",
                    ));
                };
                if let Err(error) =
                    inherit_projection_root_metadata(selected_root, &allocation.upper_dir)
                {
                    let _ = destroy_workspace_allocation(
                        &allocations_root,
                        &allocation.descriptor.allocation_id,
                        &lease.deleter,
                    );
                    return Err(mpla_error(
                        &workspace_session_id,
                        "inherit projection root metadata",
                        error,
                    ));
                }
            }
            timings.projection_metadata_elapsed_ns = elapsed_ns(projection_metadata_started);
            let external_session_prepare_started = Instant::now();
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
            timings.external_session_prepare_elapsed_ns =
                elapsed_ns(external_session_prepare_started);
            let durability_commit_started = Instant::now();
            let durability_result = if commit_object_graph {
                durability_batch.commit(&[
                    &allocation.allocation_root,
                    prepared.session_dir(),
                    &roots.control_root,
                ])
            } else {
                durability_batch.discard();
                Ok(())
            };
            durability_result.map_err(|error| {
                mpla_error(
                    &workspace_session_id,
                    "commit fresh allocation durability batch",
                    error,
                )
            })?;
            timings.durability_commit_elapsed_ns = elapsed_ns(durability_commit_started);
            let external_overlay = ExternalOverlayLayout {
                workspace_root: prepared.workspace_root().to_path_buf(),
                upperdir: allocation.upper_dir.clone(),
                workdir: allocation.work_dir.clone(),
                lower_dirs_newest_first: requested_lower_dirs_newest_first,
            };
            let workspace_create_started = Instant::now();
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
            timings.workspace_create_elapsed_ns = elapsed_ns(workspace_create_started);
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
            let launch_material_started = Instant::now();
            let lower_dirs_newest_first = match handle.entry() {
                Ok(entry) => entry.layer_paths,
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
                    return Err(mpla_error(
                        &workspace_session_id,
                        "resolve external overlay launch material",
                        error,
                    ));
                }
            };
            timings.launch_material_elapsed_ns = elapsed_ns(launch_material_started);
            let cgroup_prepare_started = Instant::now();
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
            timings.cgroup_prepare_elapsed_ns = elapsed_ns(cgroup_prepare_started);
            let binding = MplaWorkspaceBinding {
                run_id,
                payload_root: roots.payload_root,
                control_root: roots.control_root,
                storage_admin_profile: roots.storage_admin_profile,
                allocation,
                lease,
                lease_operation_id: operation_id,
                prepared,
                selected_ref: projection.map(|(selected_ref, _)| selected_ref),
                lower_dirs_newest_first,
                mount_scope: None,
                mount_receipt_binding: None,
                cleanup_operation_id: None,
                phase: MplaStoragePhase::Prepared,
            };
            let session =
                WorkspaceSession::from_mpla_handle(handle.clone(), cgroup_path.clone(), binding);
            let handler = session.handler();
            let session_register_started = Instant::now();
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
            timings.session_register_elapsed_ns = elapsed_ns(session_register_started);
            self.obs().event(
                sandbox_observability_telemetry::record::names::LEASE_ACQUIRED,
                json!({ "revision": handler.handle.base_revision().version, "mpla": true }),
            );
            let session_commit_started = Instant::now();
            self.commit_created_session(&handler)?;
            timings.session_commit_elapsed_ns = elapsed_ns(session_commit_started);
            Ok((handler, timings))
        })
    }
}

/// The first MPLA holder must have a real, explicit lower directory.  Without
/// one, workspace creation could substitute the ordinary LayerStack snapshot,
/// producing a live view that cannot be reconstructed from persisted MPLA
/// allocations.  This directory is server-owned beside the allocation's upper
/// and work directories and is never exposed inside the mounted workspace.
fn requested_mpla_lower_dirs(
    projection_lower_dirs: Option<&[PathBuf]>,
    allocation_root: &Path,
) -> std::io::Result<Vec<PathBuf>> {
    if let Some(lower_dirs) = projection_lower_dirs {
        return Ok(lower_dirs.to_vec());
    }
    let lower_dir = allocation_root.join("initial-empty-lower");
    std::fs::create_dir(&lower_dir)?;
    std::fs::File::open(allocation_root)?.sync_all()?;
    Ok(vec![lower_dir])
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

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::requested_mpla_lower_dirs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn initial_mpla_overlay_gets_a_private_empty_lower_not_a_legacy_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "mpla-initial-empty-lower-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("create isolated allocation root");

        let lower_dirs = requested_mpla_lower_dirs(None, &root)
            .expect("create explicit initial lower directory");

        assert_eq!(lower_dirs, vec![root.join("initial-empty-lower")]);
        assert!(lower_dirs[0].is_dir());
        assert!(std::fs::read_dir(&lower_dirs[0])
            .expect("read private lower directory")
            .next()
            .is_none());
        assert_ne!(lower_dirs, vec![PathBuf::from("/ordinary/layer-stack")]);

        std::fs::remove_dir_all(root).expect("remove isolated allocation root");
    }

    #[test]
    fn reactivated_mpla_overlay_keeps_its_exact_persisted_lower_stack() {
        let persisted = vec![
            PathBuf::from("/payload/allocations/newer/upper"),
            PathBuf::from("/payload/allocations/base/upper"),
        ];

        assert_eq!(
            requested_mpla_lower_dirs(Some(&persisted), std::path::Path::new("/unused"))
                .expect("preserve exact projection"),
            persisted
        );
    }
}
