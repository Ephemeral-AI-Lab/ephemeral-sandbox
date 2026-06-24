use std::collections::HashMap;
use std::time::Instant;

use crate::lifecycle::leases::{monotonic_seconds, next_handle_id};
use crate::lifecycle::remount::WorkspaceRemountState;
use crate::model::WorkspaceProfile;
use crate::namespace::NamespacePlan;
use crate::overlay::dirs::create_overlay_dirs;
use crate::profile::manager::WorkspaceModeError;
use crate::profile::{
    validate_workspace_root, WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeManager,
    WorkspaceModeSnapshot,
};

impl WorkspaceModeManager {
    pub(crate) fn initialize_handle(
        &mut self,
        handle: &mut WorkspaceModeHandle,
    ) -> Result<HashMap<String, f64>, WorkspaceModeError> {
        let layer_paths = handle.layer_paths.clone();
        let namespace_plan = match handle.profile {
            WorkspaceProfile::HostCompatible => NamespacePlan::shared_network(),
            WorkspaceProfile::Isolated => NamespacePlan::isolated(),
        };
        let mut phases_ms = HashMap::new();

        let mut phase_start = Instant::now();
        handle.holder_pid =
            self.runtime
                .spawn_ns_holder(handle, self.caps.setup_timeout_s, namespace_plan)?;
        record_create_phase_ms(&mut phases_ms, "spawn_ns_holder", phase_start);

        phase_start = Instant::now();
        handle.ns_fds = self
            .runtime
            .open_ns_fds(handle.holder_pid, namespace_plan)?;
        record_create_phase_ms(&mut phases_ms, "open_ns_fds", phase_start);

        if handle.profile == WorkspaceProfile::Isolated {
            self.setup_isolated_network_after_namespace(handle, &mut phases_ms)?;
        }

        phase_start = Instant::now();
        self.runtime.mount_overlay(handle, &layer_paths)?;
        record_create_phase_ms(&mut phases_ms, "mount_overlay", phase_start);

        if handle.profile == WorkspaceProfile::Isolated {
            self.setup_isolated_network_after_mount(handle)?;
        }

        Ok(phases_ms)
    }

    pub(crate) fn rollback_partial(&mut self, handle: &WorkspaceModeHandle) {
        let _ = self.teardown_handle(handle, 1.0);
    }

    fn setup_isolated_network_after_namespace(
        &mut self,
        handle: &mut WorkspaceModeHandle,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), WorkspaceModeError> {
        let phase_start = Instant::now();
        self.network.initialize()?;
        let veth = self
            .network
            .install_veth(&handle.workspace_id.0, handle.holder_pid)?;
        handle.veth = Some(veth);
        record_create_phase_ms(phases_ms, "install_veth", phase_start);
        Ok(())
    }

    fn setup_isolated_network_after_mount(
        &mut self,
        handle: &WorkspaceModeHandle,
    ) -> Result<(), WorkspaceModeError> {
        self.runtime
            .signal_net_ready(handle, self.caps.setup_timeout_s)
    }

    pub fn enter_with_profile(
        &mut self,
        snapshot: WorkspaceModeSnapshot,
        profile: WorkspaceProfile,
    ) -> Result<WorkspaceModeHandle, WorkspaceModeError> {
        let workspace_root = self.validated_workspace_root()?;
        self.check_host_capacity()?;

        let workspace_id = WorkspaceModeId(next_handle_id());
        let dirs =
            create_overlay_dirs(self.workspace_session_root(&workspace_id)).map_err(|err| {
                WorkspaceModeError::SetupFailed {
                    step: format!("create overlay scratch: {err}"),
                }
            })?;

        let now = monotonic_seconds();
        let mut handle = WorkspaceModeHandle {
            workspace_id: workspace_id.clone(),
            profile,
            lease_id: snapshot.lease_id,
            manifest_version: snapshot.manifest_version,
            manifest_root_hash: snapshot.manifest_root_hash,
            base_manifest: snapshot.base_manifest,
            workspace_root,
            dirs,
            layer_paths: snapshot.layer_paths,
            ns_fds: Default::default(),
            holder_pid: 0,
            readiness_fd: -1,
            control_fd: -1,
            veth: None,
            remount_state: WorkspaceRemountState::Active,
            created_at: now,
            last_activity: now,
        };

        if let Err(err) = self.initialize_handle(&mut handle) {
            self.rollback_partial(&handle);
            return Err(err);
        }

        self.handles.insert(workspace_id.clone(), handle.clone());
        if let Err(err) = self.persist_handles() {
            self.handles.remove(&workspace_id);
            self.rollback_partial(&handle);
            return Err(err);
        }
        Ok(handle)
    }

    pub(crate) fn validated_workspace_root(&self) -> Result<String, WorkspaceModeError> {
        let workspace_root = self.workspace_root.trim();
        validate_workspace_root(workspace_root)?;
        Ok(workspace_root.to_owned())
    }
}

fn record_create_phase_ms(
    phases_ms: &mut HashMap<String, f64>,
    phase: &'static str,
    started_at: Instant,
) {
    let duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    phases_ms.insert(phase.to_owned(), duration_ms);
}
