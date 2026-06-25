use std::path::Path;

use crate::error::WorkspaceError;
use crate::model::WorkspaceHandle;
use crate::profile::{WorkspaceModeError, WorkspaceModeId, WorkspaceModeSnapshot};
use crate::service::WorkspaceRuntimeState;

pub(crate) fn ensure_absolute(path: &Path, field: &'static str) -> Result<(), WorkspaceError> {
    if !path.is_absolute() {
        return Err(WorkspaceError::InvalidRequest {
            field,
            message: format!("must be absolute: {}", path.display()),
        });
    }
    Ok(())
}

pub(crate) fn workspace_error_from_mode_error(error: WorkspaceModeError) -> WorkspaceError {
    match error {
        WorkspaceModeError::InvalidArgument(message) => WorkspaceError::InvalidRequest {
            field: "workspace",
            message,
        },
        WorkspaceModeError::NotOpen => WorkspaceError::NotOpen,
        WorkspaceModeError::SetupFailed { step } => WorkspaceError::Setup { step },
        WorkspaceModeError::NetworkUnavailable(message) => WorkspaceError::Network { message },
    }
}

pub(crate) fn active_mode_id(
    state: &WorkspaceRuntimeState,
    handle: &WorkspaceHandle,
) -> Result<WorkspaceModeId, WorkspaceError> {
    let mode_id = WorkspaceModeId(handle.id.0.clone());
    let Some(mode_handle) = state.manager.handles.get(&mode_id) else {
        return Err(WorkspaceError::NotOpen);
    };
    let _ = mode_handle;
    Ok(mode_id)
}

pub(crate) fn mode_snapshot_from_layerstack(
    snapshot: sandbox_runtime_layerstack::service::LeasedSnapshot,
) -> WorkspaceModeSnapshot {
    WorkspaceModeSnapshot {
        lease_id: snapshot.lease_id,
        manifest_version: snapshot.manifest_version,
        manifest_root_hash: snapshot.root_hash,
        base_manifest: snapshot.manifest,
        layer_paths: snapshot.layer_paths,
    }
}
