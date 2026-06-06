use std::path::PathBuf;

use eos_ephemeral_workspace::command_session::types::{
    EphemeralCommandPrepareContext, EphemeralCommandSessionPort,
};
use eos_ephemeral_workspace::{
    EphemeralSnapshot, EphemeralWorkspaceError, PathChange, PublishOutcome, WorkspacePublisherPort,
    WorkspaceRoot,
};
use eos_layerstack::LayerStack;
use eos_protocol::LayerChange;
use eos_workspace_api::{WorkspaceApiError, WorkspaceTimings};

use crate::response_timings::{resource_timings, timing_map};
use crate::services::overlay::{ephemeral_dir_allocator, DaemonPublisherPort};

pub(in crate::services::command_session) struct DaemonEphemeralCommandPort {
    root: PathBuf,
    workspace_root: PathBuf,
    scratch_root: PathBuf,
}

impl DaemonEphemeralCommandPort {
    pub(in crate::services::command_session) fn new(
        root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
    ) -> Self {
        Self {
            root,
            workspace_root,
            scratch_root,
        }
    }
}

impl EphemeralCommandSessionPort for DaemonEphemeralCommandPort {
    fn prepare_context(
        &self,
        command_session_id: &str,
    ) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError> {
        let session_dir = self.scratch_root.join(command_session_id);
        Ok(EphemeralCommandPrepareContext {
            layer_stack_root: self.root.clone(),
            workspace_root: self.workspace_root.clone(),
            writable_root: ephemeral_dir_allocator()
                .map_err(workspace_api_error)?
                .writable_root,
            final_path: session_dir.join("final.json"),
            session_dir,
        })
    }

    fn acquire_snapshot(
        &self,
        request_id: &str,
    ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
        let lease = LayerStack::open(self.root.clone())
            .and_then(|stack| stack.acquire_snapshot(request_id))
            .map_err(|error| EphemeralWorkspaceError::SnapshotAcquire {
                reason: error.to_string(),
            })?;
        Ok(EphemeralSnapshot {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        })
    }

    fn release_snapshot(&self, lease_id: &str) -> Result<(), EphemeralWorkspaceError> {
        LayerStack::open(self.root.clone())
            .and_then(|mut stack| stack.release_lease(lease_id))
            .map(|_| ())
            .map_err(|error| EphemeralWorkspaceError::LeaseRelease {
                lease_id: lease_id.to_owned(),
                reason: error.to_string(),
            })
    }

    fn base_timings(&self) -> Result<WorkspaceTimings, WorkspaceApiError> {
        let manifest = LayerStack::open(self.root.clone())
            .and_then(|stack| stack.read_active_manifest())
            .map_err(workspace_api_error)?;
        Ok(timing_map(resource_timings(&manifest, 0)))
    }

    fn publish_upperdir_changes(
        &self,
        root: &WorkspaceRoot,
        snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError> {
        DaemonPublisherPort::new(&self.root)
            .publish_upperdir_changes(root, snapshot, changes, path_kinds)
    }
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
}
