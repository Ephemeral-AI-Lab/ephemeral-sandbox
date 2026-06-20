use std::collections::HashMap;
use std::time::Instant;

use crate::lifecycle::remount::WorkspaceRemountState;
use crate::lifecycle::{
    leases::{monotonic_seconds, next_handle_id},
    record_phase_ms,
};
use crate::model::WorkspaceProfile;
use crate::namespace::NamespacePlan;
use crate::overlay::dirs::create_overlay_dirs;
use crate::profile::manager::IsolatedNetworkError;
use crate::profile::{
    WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeManager, WorkspaceModeSnapshot,
};

impl WorkspaceModeManager {
    pub(crate) fn initialize_handle(
        &mut self,
        handle: &mut WorkspaceModeHandle,
    ) -> Result<HashMap<String, f64>, IsolatedNetworkError> {
        let layer_paths = handle.layer_paths.clone();
        let namespace_plan = match handle.profile {
            WorkspaceProfile::SharedNetwork => NamespacePlan::shared_network(),
            WorkspaceProfile::Isolated => NamespacePlan::isolated(),
        };
        let mut phases_ms = HashMap::new();

        let mut phase_start = Instant::now();
        handle.holder_pid =
            self.runtime
                .spawn_ns_holder(handle, self.caps.setup_timeout_s, namespace_plan)?;
        record_phase_ms(&mut phases_ms, "spawn_ns_holder", phase_start);

        phase_start = Instant::now();
        handle.ns_fds = self
            .runtime
            .open_ns_fds(handle.holder_pid, namespace_plan)?;
        record_phase_ms(&mut phases_ms, "open_ns_fds", phase_start);

        if handle.profile == WorkspaceProfile::Isolated {
            self.setup_isolated_network_after_namespace(handle, &mut phases_ms)?;
        }

        phase_start = Instant::now();
        self.runtime
            .mount_overlay(handle, &layer_paths, self.caps.setup_timeout_s)?;
        record_phase_ms(&mut phases_ms, "mount_overlay", phase_start);

        if handle.profile == WorkspaceProfile::Isolated {
            self.setup_isolated_network_after_mount(handle, &mut phases_ms)?;
        }

        phase_start = Instant::now();
        let cgroup_path = self.runtime.create_cgroup(handle)?;
        record_phase_ms(&mut phases_ms, "create_cgroup", phase_start);
        if !cgroup_path.as_os_str().is_empty() {
            handle.cgroup_path = Some(cgroup_path);
        }
        phase_start = Instant::now();
        self.runtime.join_holder_cgroup(handle)?;
        record_phase_ms(&mut phases_ms, "join_holder_cgroup", phase_start);
        Ok(phases_ms)
    }

    pub(crate) fn rollback_partial(&mut self, handle: &WorkspaceModeHandle) {
        let _ = self.teardown_handle(handle, 1.0);
    }

    fn setup_isolated_network_after_namespace(
        &mut self,
        handle: &mut WorkspaceModeHandle,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        let phase_start = Instant::now();
        let veth = if self.runtime.stub {
            self.network.install_stub_veth(&handle.workspace_id.0)?
        } else {
            self.network.initialize()?;
            self.network
                .install_veth(&handle.workspace_id.0, handle.holder_pid)?
        };
        handle.veth = Some(veth);
        record_phase_ms(phases_ms, "install_veth", phase_start);
        Ok(())
    }

    fn setup_isolated_network_after_mount(
        &mut self,
        handle: &mut WorkspaceModeHandle,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        let phase_start = Instant::now();
        handle.dns_configuration = self.runtime.configure_dns(
            handle,
            &self.caps.fallback_dns,
            self.caps.setup_timeout_s,
        )?;
        record_phase_ms(phases_ms, "configure_dns", phase_start);
        self.runtime
            .signal_net_ready(handle, self.caps.setup_timeout_s)
    }

    pub fn enter_with_profile(
        &mut self,
        snapshot: WorkspaceModeSnapshot,
        profile: WorkspaceProfile,
    ) -> Result<WorkspaceModeHandle, IsolatedNetworkError> {
        let workspace_root = self.validated_workspace_root()?;
        let total_cap = usize::try_from(self.caps.total_cap).unwrap_or(usize::MAX);
        if self.handles.len() >= total_cap {
            return Err(IsolatedNetworkError::QuotaExceeded {
                total_cap: self.caps.total_cap,
            });
        }
        self.check_host_capacity()?;

        let workspace_id = WorkspaceModeId(next_handle_id());
        let dirs = create_overlay_dirs(self.owned_scratch_root().join(&workspace_id.0)).map_err(
            |err| IsolatedNetworkError::SetupFailed {
                step: format!("create overlay scratch: {err}"),
            },
        )?;

        let now = monotonic_seconds();
        let mut handle = WorkspaceModeHandle {
            workspace_id: workspace_id.clone(),
            profile,
            lease_id: snapshot.lease_id,
            manifest_version: snapshot.manifest_version,
            manifest_root_hash: snapshot.manifest_root_hash,
            workspace_root,
            dirs,
            layer_paths: snapshot.layer_paths,
            ns_fds: Default::default(),
            holder_pid: 0,
            readiness_fd: -1,
            control_fd: -1,
            veth: None,
            cgroup_path: None,
            dns_configuration: Default::default(),
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

    pub(crate) fn validated_workspace_root(&self) -> Result<String, IsolatedNetworkError> {
        let workspace_root = self.caps.eos_workspace_root.trim();
        if workspace_root.is_empty() {
            return Err(IsolatedNetworkError::InvalidArgument(
                "eos_workspace_root is required".to_owned(),
            ));
        }
        if !std::path::Path::new(workspace_root).is_absolute() {
            return Err(IsolatedNetworkError::InvalidArgument(format!(
                "eos_workspace_root must be absolute: {workspace_root}"
            )));
        }
        Ok(workspace_root.to_owned())
    }
}
