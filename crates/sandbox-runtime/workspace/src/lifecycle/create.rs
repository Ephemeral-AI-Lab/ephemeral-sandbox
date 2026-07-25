use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::lifecycle::leases::monotonic_seconds;
use crate::model::{LayerStackSnapshotRef, NetworkProfile, WorkspaceSessionId};
use crate::namespace::NamespacePlan;
use crate::overlay::dirs::{create_overlay_dirs, OverlayDirs};
use crate::session::manager::WorkspaceManagerError;
use crate::session::{validate_workspace_root, MountedWorkspace, WorkspaceManager};

impl WorkspaceManager {
    pub(crate) fn initialize_handle(
        &mut self,
        handle: &mut MountedWorkspace,
    ) -> Result<HashMap<String, f64>, WorkspaceManagerError> {
        let layer_paths = handle.snapshot.layer_paths.clone();
        let namespace_plan = match handle.network {
            NetworkProfile::Shared => NamespacePlan::shared_network(),
            NetworkProfile::Isolated => NamespacePlan::isolated(),
        };
        let mut phases_ms = HashMap::new();

        let mut phase_start = Instant::now();
        handle.holder_pid =
            self.runtime
                .spawn_ns_holder(handle, self.caps.setup_timeout_s, namespace_plan)?;
        super::record_phase_ms(&mut phases_ms, "spawn_ns_holder", phase_start);

        phase_start = Instant::now();
        handle.ns_fds = self
            .runtime
            .open_ns_fds(handle.holder_pid, namespace_plan)?;
        super::record_phase_ms(&mut phases_ms, "open_ns_fds", phase_start);

        if handle.network == NetworkProfile::Isolated {
            self.setup_isolated_network_after_namespace(handle, &mut phases_ms)?;
        }

        phase_start = Instant::now();
        self.runtime.mount_overlay(handle, &layer_paths)?;
        super::record_phase_ms(&mut phases_ms, "mount_overlay", phase_start);

        if handle.network == NetworkProfile::Isolated {
            self.setup_isolated_network_after_mount(handle)?;
        }

        Ok(phases_ms)
    }

    pub(crate) fn rollback_partial(
        &mut self,
        handle: &MountedWorkspace,
    ) -> Result<crate::session::ExitOutcome, WorkspaceManagerError> {
        self.rollback_unpublished(handle)
    }

    fn fail_after_partial_create(
        &mut self,
        handle: &MountedWorkspace,
        create_error: WorkspaceManagerError,
    ) -> WorkspaceManagerError {
        match self.rollback_partial(handle) {
            Ok(_) => create_error,
            Err(WorkspaceManagerError::TeardownFailed {
                workspace_session_id,
                mut failures,
            }) => {
                failures.insert(0, format!("CreateSetup: {create_error}"));
                WorkspaceManagerError::TeardownFailed {
                    workspace_session_id,
                    failures,
                }
            }
            Err(rollback_error) => WorkspaceManagerError::TeardownFailed {
                workspace_session_id: handle.workspace_id.clone(),
                failures: vec![
                    format!("CreateSetup: {create_error}"),
                    format!("Rollback: {rollback_error}"),
                ],
            },
        }
    }

    fn setup_isolated_network_after_namespace(
        &mut self,
        handle: &mut MountedWorkspace,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), WorkspaceManagerError> {
        let phase_start = Instant::now();
        self.network.initialize()?;
        let veth = self
            .network
            .install_veth(&handle.workspace_id.0, handle.holder_pid)?;
        handle.veth = Some(veth);
        super::record_phase_ms(phases_ms, "install_veth", phase_start);
        Ok(())
    }

    fn setup_isolated_network_after_mount(
        &mut self,
        handle: &MountedWorkspace,
    ) -> Result<(), WorkspaceManagerError> {
        self.runtime
            .signal_net_ready(handle, self.caps.setup_timeout_s)
    }

    pub fn open(
        &mut self,
        workspace_id: WorkspaceSessionId,
        snapshot: LayerStackSnapshotRef,
        network: NetworkProfile,
    ) -> Result<MountedWorkspace, WorkspaceManagerError> {
        self.open_with_candidate(workspace_id, snapshot, network, None, None)
    }

    pub(crate) fn open_with_candidate(
        &mut self,
        workspace_id: WorkspaceSessionId,
        snapshot: LayerStackSnapshotRef,
        network: NetworkProfile,
        candidate_admission: Option<
            sandbox_runtime_layerstack::service::CandidateGenerationAdmission,
        >,
        candidate_session_lease_ttl: Option<Duration>,
    ) -> Result<MountedWorkspace, WorkspaceManagerError> {
        self.ensure_workspace_available(&workspace_id)?;
        let run_dir = self.workspace_session_root(&workspace_id);
        let dirs = OverlayDirs {
            upperdir: run_dir.join("upper"),
            workdir: run_dir.join("work"),
            run_dir,
        };
        let now = monotonic_seconds();
        let mut handle = MountedWorkspace {
            workspace_id: workspace_id.clone(),
            network,
            snapshot,
            candidate_admission,
            workspace_root: self.workspace_root.trim().to_owned(),
            dirs,
            ns_fds: Default::default(),
            holder_pid: 0,
            holder_registration: crate::namespace::holder::HolderRegistration::unmanaged(
                workspace_id.clone(),
                0,
            ),
            readiness_fd: -1,
            control_fd: -1,
            veth: None,
            created_at: now,
            last_activity: now,
            parked_lease_id: None,
        };

        match self.validated_workspace_root() {
            Ok(workspace_root) => handle.workspace_root = workspace_root,
            Err(error) => return Err(self.fail_after_partial_create(&handle, error)),
        }
        if handle.candidate_admission.is_some() {
            self.handles.insert(workspace_id.clone(), handle.clone());
            if let Err(error) = self.persist_handles() {
                self.handles.remove(&workspace_id);
                return Err(self.fail_after_partial_create(&handle, error));
            }
            self.handles.remove(&workspace_id);
        }
        if let Err(error) = self.scratch_locator.ensure_session(&workspace_id) {
            let error = WorkspaceManagerError::SetupFailed {
                step: format!("create workspace scratch: {error}"),
            };
            return Err(self.fail_after_partial_create(&handle, error));
        }
        match create_overlay_dirs(handle.dirs.run_dir.clone()) {
            Ok(dirs) => handle.dirs = dirs,
            Err(error) => {
                let error = WorkspaceManagerError::SetupFailed {
                    step: format!("create overlay scratch: {error}"),
                };
                return Err(self.fail_after_partial_create(&handle, error));
            }
        }

        if let Err(err) = self.initialize_handle(&mut handle) {
            return Err(self.fail_after_partial_create(&handle, err));
        }

        if handle.candidate_admission.is_some() {
            let Some(lease_ttl) = candidate_session_lease_ttl else {
                let error = WorkspaceManagerError::InvalidArgument(
                    "candidate session lease duration is required".to_owned(),
                );
                return Err(self.fail_after_partial_create(&handle, error));
            };
            let Some(layer_stack_root) = self.layer_stack_root.clone() else {
                let error = WorkspaceManagerError::SetupFailed {
                    step: "layer stack root is not bound to workspace manager".to_owned(),
                };
                return Err(self.fail_after_partial_create(&handle, error));
            };
            let lease = &handle
                .candidate_admission
                .as_ref()
                .expect("candidate admission was checked")
                .lease;
            let renewed =
                match sandbox_runtime_layerstack::service::finalize_hidden_candidate_session(
                    &layer_stack_root,
                    lease,
                    lease_ttl,
                ) {
                    Ok(lease) => lease,
                    Err(error) => {
                        let error = WorkspaceManagerError::SetupFailed {
                            step: format!("finalize strict candidate session: {error}"),
                        };
                        return Err(self.fail_after_partial_create(&handle, error));
                    }
                };
            handle
                .candidate_admission
                .as_mut()
                .expect("candidate admission was checked")
                .lease = renewed;
        } else if candidate_session_lease_ttl.is_some() {
            let error = WorkspaceManagerError::InvalidArgument(
                "candidate session lease duration requires an admission".to_owned(),
            );
            return Err(self.fail_after_partial_create(&handle, error));
        }

        self.handles.insert(workspace_id.clone(), handle.clone());
        if let Err(err) = self.persist_handles() {
            self.handles.remove(&workspace_id);
            return Err(self.fail_after_partial_create(&handle, err));
        }
        Ok(handle)
    }

    pub(crate) fn validated_workspace_root(&self) -> Result<String, WorkspaceManagerError> {
        let workspace_root = self.workspace_root.trim();
        validate_workspace_root(workspace_root)?;
        Ok(workspace_root.to_owned())
    }
}
