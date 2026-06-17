use std::collections::HashMap;
use std::time::Instant;

use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::{
    IsolatedManager, IsolatedSnapshot, IsolatedWorkspaceId, WorkspaceHandle,
};
use crate::lifecycle::leases::{monotonic_seconds, next_handle_id};
use crate::lifecycle::remount::WorkspaceRemountState;
use crate::overlay::dirs::create_overlay_dirs;

use super::{close_handle_fds, record_phase_ms};

impl IsolatedManager {
    pub(crate) fn wire_handle(
        &mut self,
        handle: &mut WorkspaceHandle,
    ) -> Result<HashMap<String, f64>, IsolatedError> {
        let mut phases_ms = HashMap::new();
        let mut phase_start = Instant::now();
        handle.holder_pid = self
            .runtime
            .spawn_ns_holder(handle, self.caps.setup_timeout_s)?;
        record_phase_ms(&mut phases_ms, "spawn_ns_holder", phase_start);
        phase_start = Instant::now();
        handle.ns_fds = self.runtime.open_ns_fds(handle.holder_pid)?;
        record_phase_ms(&mut phases_ms, "open_ns_fds", phase_start);
        phase_start = Instant::now();
        self.network.initialize()?;
        handle.veth = Some(
            self.network
                .install_veth(&handle.workspace_id.0, handle.holder_pid)?,
        );
        record_phase_ms(&mut phases_ms, "install_veth", phase_start);
        phase_start = Instant::now();
        self.runtime.mount_overlay(
            handle,
            &handle.layer_paths.clone(),
            self.caps.setup_timeout_s,
        )?;
        record_phase_ms(&mut phases_ms, "mount_overlay", phase_start);
        phase_start = Instant::now();
        handle.dns_configuration = self.runtime.configure_dns(
            handle,
            &self.caps.fallback_dns,
            self.caps.setup_timeout_s,
        )?;
        record_phase_ms(&mut phases_ms, "configure_dns", phase_start);
        self.runtime
            .signal_net_ready(handle, self.caps.setup_timeout_s)?;
        phase_start = Instant::now();
        let cgroup_path = self.runtime.create_cgroup(handle)?;
        record_phase_ms(&mut phases_ms, "create_cgroup", phase_start);
        if !cgroup_path.as_os_str().is_empty() {
            handle.cgroup_path = Some(cgroup_path);
        }
        Ok(phases_ms)
    }

    pub(crate) fn rollback_partial(&mut self, handle: &WorkspaceHandle) {
        close_handle_fds(handle);
        if let Some(veth) = handle.veth.as_ref() {
            self.network.teardown_veth(veth);
        }
        if handle.holder_pid > 0 {
            let _ = self.runtime.kill_holder(handle.holder_pid, 1.0);
        }
        let _ = std::fs::remove_dir_all(&handle.dirs.run_dir);
    }

    pub fn enter(
        &mut self,
        caller_id: &str,
        snapshot: IsolatedSnapshot,
    ) -> Result<WorkspaceHandle, IsolatedError> {
        if !self.caps.enabled {
            return Err(IsolatedError::FeatureDisabled);
        }
        if caller_id.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "caller_id is required".to_owned(),
            ));
        }
        let workspace_root = self.validated_workspace_root()?;
        if self.by_caller.contains_key(caller_id) {
            let existing = self
                .by_caller
                .get(caller_id)
                .and_then(|workspace_id| self.handles.get(workspace_id))
                .ok_or_else(|| IsolatedError::SetupFailed {
                    step: "agent handle index is inconsistent".to_owned(),
                })?;
            return Err(IsolatedError::AlreadyOpen {
                created_at: existing.created_at,
                last_activity: existing.last_activity,
            });
        }
        let total_cap = usize::try_from(self.caps.total_cap).unwrap_or(usize::MAX);
        if self.handles.len() >= total_cap {
            return Err(IsolatedError::QuotaExceeded {
                total_cap: self.caps.total_cap,
            });
        }
        self.check_host_capacity()?;

        let workspace_id = IsolatedWorkspaceId(next_handle_id());
        let dirs = create_overlay_dirs(self.owned_scratch_root().join(&workspace_id.0)).map_err(
            |err| IsolatedError::SetupFailed {
                step: format!("{}: {}", err.path.display(), err.reason),
            },
        )?;

        let now = monotonic_seconds();
        let mut handle = WorkspaceHandle {
            workspace_id: workspace_id.clone(),
            caller_id: caller_id.to_owned(),
            lease_id: snapshot.lease_id,
            manifest_version: snapshot.manifest_version,
            manifest_root_hash: snapshot.manifest_root_hash,
            workspace_root,
            dirs,
            layer_paths: snapshot.layer_paths,
            ns_fds: HashMap::new(),
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

        if let Err(err) = self.wire_handle(&mut handle) {
            self.rollback_partial(&handle);
            return Err(err);
        }

        self.by_caller
            .insert(caller_id.to_owned(), workspace_id.clone());
        self.handles.insert(workspace_id.clone(), handle.clone());
        if let Err(err) = self.persist_handles() {
            self.by_caller.remove(caller_id);
            self.handles.remove(&workspace_id);
            self.rollback_partial(&handle);
            return Err(err);
        }
        Ok(handle)
    }

    pub(crate) fn validated_workspace_root(&self) -> Result<String, IsolatedError> {
        let workspace_root = self.caps.eos_workspace_root.trim();
        if workspace_root.is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "eos_workspace_root is required".to_owned(),
            ));
        }
        if !std::path::Path::new(workspace_root).is_absolute() {
            return Err(IsolatedError::InvalidArgument(format!(
                "eos_workspace_root must be absolute: {workspace_root}"
            )));
        }
        Ok(workspace_root.to_owned())
    }
}
