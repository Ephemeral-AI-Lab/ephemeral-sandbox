use std::path::PathBuf;

use eos_protocol::LayerChange;
use eos_workspace_api::{
    FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome, WorkspaceTimings,
};

use crate::{
    EphemeralSnapshot, EphemeralWorkspace, EphemeralWorkspaceError, PathChange, PublishOutcome,
    WorkspaceRoot,
};

/// Daemon-supplied facts needed to prepare a publishable command workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralCommandPrepareContext {
    pub layer_stack_root: PathBuf,
    pub workspace_root: PathBuf,
    pub writable_root: PathBuf,
    pub session_dir: PathBuf,
    pub final_path: PathBuf,
}

/// Daemon-supplied facts needed to finalize a publishable command workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct EphemeralCommandFinalizeContext {
    pub workspace: EphemeralWorkspace,
    pub base_timings: WorkspaceTimings,
}

/// Daemon-supplied port for ephemeral command-session prepare/finalize policy.
///
/// The port keeps PTY/process/session registry ownership in `eos-daemon` while
/// allowing this crate to compile against the shared `CommandWorkspaceOps`
/// contract.
pub trait EphemeralCommandSessionPort {
    fn prepare_context(&self) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError> {
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "ephemeral adapter cannot prepare command workspaces",
        ))
    }

    fn acquire_snapshot(
        &self,
        request_id: &str,
    ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
        let _ = request_id;
        Err(EphemeralWorkspaceError::SnapshotAcquire {
            reason: "ephemeral adapter cannot acquire command snapshots".to_owned(),
        })
    }

    fn release_snapshot(&self, lease_id: &str) -> Result<(), EphemeralWorkspaceError> {
        let _ = lease_id;
        Err(EphemeralWorkspaceError::LeaseRelease {
            lease_id: lease_id.to_owned(),
            reason: "ephemeral adapter cannot release command snapshots".to_owned(),
        })
    }

    fn finalize_context(&self) -> Result<EphemeralCommandFinalizeContext, WorkspaceApiError> {
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "ephemeral adapter cannot provide command finalize context",
        ))
    }

    fn publish_upperdir_changes(
        &self,
        root: &WorkspaceRoot,
        snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError> {
        let _ = (root, snapshot, changes, path_kinds);
        Err(EphemeralWorkspaceError::PublishFailed {
            reason: "ephemeral command adapter cannot publish upperdir changes".to_owned(),
        })
    }

    fn finalize_ephemeral_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        let _ = request;
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "ephemeral adapter cannot finalize command workspaces",
        ))
    }
}
