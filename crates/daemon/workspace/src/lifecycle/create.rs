use std::collections::HashMap;

use crate::lifecycle::leases::{monotonic_seconds, next_handle_id};
use crate::model::NetworkMode;
use crate::overlay::dirs::create_overlay_dirs;
use crate::profile::common::{
    new_workspace_handle, teardown_workspace, wire_workspace, WorkspaceHandleSpec, WorkspaceProfile,
};
use crate::profile::manager::IsolatedNetworkError;
use crate::profile::{
    WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeManager, WorkspaceModeSnapshot,
};

impl WorkspaceModeManager {
    pub(crate) fn wire_handle(
        &mut self,
        handle: &mut WorkspaceModeHandle,
    ) -> Result<HashMap<String, f64>, IsolatedNetworkError> {
        let layer_paths = handle.layer_paths.clone();
        let mut profile = WorkspaceProfile::for_mode(
            handle.network,
            &mut self.network,
            &self.caps.fallback_dns,
            self.caps.setup_timeout_s,
        );
        wire_workspace(
            &self.runtime,
            handle,
            &layer_paths,
            self.caps.setup_timeout_s,
            &mut profile,
        )
    }

    pub(crate) fn rollback_partial(&mut self, handle: &WorkspaceModeHandle) {
        let mut profile = WorkspaceProfile::for_mode(
            handle.network,
            &mut self.network,
            &self.caps.fallback_dns,
            self.caps.setup_timeout_s,
        );
        let _ = teardown_workspace(&self.runtime, handle, &mut profile, 1.0);
    }

    pub fn enter(
        &mut self,
        caller_id: &str,
        snapshot: WorkspaceModeSnapshot,
    ) -> Result<WorkspaceModeHandle, IsolatedNetworkError> {
        self.enter_with_network(caller_id, snapshot, NetworkMode::Isolated)
    }

    pub fn enter_with_network(
        &mut self,
        caller_id: &str,
        snapshot: WorkspaceModeSnapshot,
        network: NetworkMode,
    ) -> Result<WorkspaceModeHandle, IsolatedNetworkError> {
        if !self.caps.enabled {
            return Err(IsolatedNetworkError::FeatureDisabled);
        }
        if caller_id.trim().is_empty() {
            return Err(IsolatedNetworkError::InvalidArgument(
                "caller_id is required".to_owned(),
            ));
        }
        let workspace_root = self.validated_workspace_root()?;
        if self.by_caller.contains_key(caller_id) {
            let existing = self
                .by_caller
                .get(caller_id)
                .and_then(|workspace_id| self.handles.get(workspace_id))
                .ok_or_else(|| IsolatedNetworkError::SetupFailed {
                    step: "agent handle index is inconsistent".to_owned(),
                })?;
            return Err(IsolatedNetworkError::AlreadyOpen {
                created_at: existing.created_at,
                last_activity: existing.last_activity,
            });
        }
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
                step: format!("{}: {}", err.path.display(), err.reason),
            },
        )?;

        let now = monotonic_seconds();
        let mut handle = new_workspace_handle(WorkspaceHandleSpec {
            workspace_id: workspace_id.clone(),
            network,
            caller_id: caller_id.to_owned(),
            lease_id: snapshot.lease_id,
            manifest_version: snapshot.manifest_version,
            manifest_root_hash: snapshot.manifest_root_hash,
            workspace_root,
            dirs,
            layer_paths: snapshot.layer_paths,
            created_at: now,
            last_activity: now,
        });

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
