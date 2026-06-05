use std::path::PathBuf;

use eos_protocol::LayerChange;
use eos_workspace_api::{WorkspaceApiError, WorkspaceTimings};

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
/// allowing this crate to compile against the shared `CommandWorkspacePolicy`
/// contract.
pub trait EphemeralCommandSessionPort {
    fn prepare_context(
        &self,
        command_session_id: &str,
    ) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError>;

    fn acquire_snapshot(
        &self,
        request_id: &str,
    ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError>;

    fn release_snapshot(&self, lease_id: &str) -> Result<(), EphemeralWorkspaceError>;

    fn base_timings(&self) -> Result<WorkspaceTimings, WorkspaceApiError>;

    fn publish_upperdir_changes(
        &self,
        root: &WorkspaceRoot,
        snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError>;
}
