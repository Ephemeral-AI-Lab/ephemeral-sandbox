use std::path::PathBuf;

use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::IsolatedManager;
use crate::lifecycle::leases::monotonic_seconds;

use super::{RemountProbe, RemountedWorkspace, WorkspaceRemountState};

impl IsolatedManager {
    pub fn mark_remount_pending(&mut self, caller_id: &str) -> Result<(), IsolatedError> {
        self.set_remount_state(caller_id, WorkspaceRemountState::Pending)
    }

    pub fn clear_remount_pending(&mut self, caller_id: &str) -> Result<(), IsolatedError> {
        self.set_remount_state(caller_id, WorkspaceRemountState::Active)
    }

    fn set_remount_state(
        &mut self,
        caller_id: &str,
        remount_state: WorkspaceRemountState,
    ) -> Result<(), IsolatedError> {
        if caller_id.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "caller_id is required".to_owned(),
            ));
        }
        let workspace_id = self
            .by_caller
            .get(caller_id)
            .cloned()
            .ok_or(IsolatedError::NotOpen)?;
        let handle = self
            .handles
            .get_mut(&workspace_id)
            .ok_or(IsolatedError::NotOpen)?;
        if handle.remount_state == remount_state {
            return Ok(());
        }
        handle.remount_state = remount_state;
        handle.last_activity = monotonic_seconds();
        self.persist_handles()
    }

    pub fn remount_with_layers(
        &mut self,
        caller_id: &str,
        manifest_version: i64,
        manifest_root_hash: String,
        layer_paths: Vec<PathBuf>,
        probe: &RemountProbe,
    ) -> Result<RemountedWorkspace, IsolatedError> {
        if caller_id.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "caller_id is required".to_owned(),
            ));
        }
        if layer_paths.is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "layer_paths must not be empty".to_owned(),
            ));
        }
        let workspace_id = self
            .by_caller
            .get(caller_id)
            .cloned()
            .ok_or(IsolatedError::NotOpen)?;
        let handle = self
            .handles
            .get(&workspace_id)
            .cloned()
            .ok_or(IsolatedError::NotOpen)?;
        let remount = self.runtime.remount_overlay(
            &handle,
            &layer_paths,
            probe,
            self.caps.setup_timeout_s,
        )?;
        if !remount.mount_verified {
            return Err(IsolatedError::SetupFailed {
                step: format!(
                    "remount overlay verification failed: {}",
                    remount.failure_summary()
                ),
            });
        }
        let updated = self
            .handles
            .get_mut(&workspace_id)
            .ok_or(IsolatedError::NotOpen)?;
        updated.manifest_version = manifest_version;
        updated.manifest_root_hash = manifest_root_hash;
        updated.layer_paths = layer_paths;
        updated.last_activity = monotonic_seconds();
        let updated = updated.clone();
        self.persist_handles()?;
        Ok(RemountedWorkspace {
            handle: updated,
            remount,
        })
    }
}
