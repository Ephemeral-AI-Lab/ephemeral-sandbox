use crate::lifecycle::leases::monotonic_seconds;
use crate::profile::{
    WorkspaceModeError, WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeManager,
};

use super::{RemountProbe, WorkspaceRemountState};

impl WorkspaceModeManager {
    pub(crate) fn remount_with_layers(
        &mut self,
        workspace_id: &WorkspaceModeId,
        layer_paths: Vec<std::path::PathBuf>,
        probe: &RemountProbe,
    ) -> Result<WorkspaceModeHandle, WorkspaceModeError> {
        if layer_paths.is_empty() {
            return Err(WorkspaceModeError::InvalidArgument(
                "layer_paths must not be empty".to_owned(),
            ));
        }
        if !self.handles.contains_key(workspace_id) {
            return Err(WorkspaceModeError::NotOpen);
        }
        self.set_remount_state(workspace_id, WorkspaceRemountState::Pending)?;
        let result = self.apply_remount(workspace_id, layer_paths, probe);
        if result.is_err() {
            let _ = self.block_remount(workspace_id);
        }
        result
    }

    pub(crate) fn block_remount(
        &mut self,
        workspace_id: &WorkspaceModeId,
    ) -> Result<(), WorkspaceModeError> {
        self.set_remount_state(workspace_id, WorkspaceRemountState::Active)
    }

    fn apply_remount(
        &mut self,
        workspace_id: &WorkspaceModeId,
        layer_paths: Vec<std::path::PathBuf>,
        probe: &RemountProbe,
    ) -> Result<WorkspaceModeHandle, WorkspaceModeError> {
        let handle = self
            .handles
            .get(workspace_id)
            .cloned()
            .ok_or(WorkspaceModeError::NotOpen)?;
        let remount = self
            .runtime
            .remount_overlay(&handle, &layer_paths, probe)?;
        if !remount.mount_verified {
            return Err(WorkspaceModeError::SetupFailed {
                step: format!(
                    "remount overlay verification failed: {}",
                    remount.failure_summary()
                ),
            });
        }
        let updated = self
            .handles
            .get_mut(workspace_id)
            .ok_or(WorkspaceModeError::NotOpen)?;
        updated.layer_paths = layer_paths;
        updated.remount_state = WorkspaceRemountState::Active;
        updated.last_activity = monotonic_seconds();
        let updated = updated.clone();
        self.persist_handles()?;
        Ok(updated)
    }

    pub(super) fn set_remount_state(
        &mut self,
        workspace_id: &WorkspaceModeId,
        remount_state: WorkspaceRemountState,
    ) -> Result<(), WorkspaceModeError> {
        let handle = self
            .handles
            .get_mut(workspace_id)
            .ok_or(WorkspaceModeError::NotOpen)?;
        if handle.remount_state == remount_state {
            return Ok(());
        }
        handle.remount_state = remount_state;
        handle.last_activity = monotonic_seconds();
        self.persist_handles()
    }
}
