use std::collections::HashMap;
use std::path::PathBuf;

use eos_workspace_api::{
    FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome, WorkspaceTimings,
};

/// Daemon-supplied facts needed to prepare an isolated command workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedCommandPrepareContext {
    pub workspace_handle_id: String,
    pub workspace_root: PathBuf,
    pub scratch_dir: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub ns_fds: HashMap<String, i32>,
    pub cgroup_path: Option<PathBuf>,
}

/// Daemon-supplied facts needed to finalize an isolated command workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct IsolatedCommandFinalizeContext {
    pub agent_id: String,
    pub workspace_handle_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub upperdir: PathBuf,
    pub base_timings: WorkspaceTimings,
}

/// Daemon-supplied port for isolated command-session prepare/finalize policy.
///
/// This port exposes no publish capability. It exists so isolated command
/// workspace policy compiles against `CommandWorkspaceOps` while daemon PTY,
/// child process, registry, and reaper control remain in `eos-daemon`.
pub trait IsolatedCommandSessionPort {
    fn prepare_context(&self) -> Result<IsolatedCommandPrepareContext, WorkspaceApiError> {
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "isolated adapter cannot prepare command workspaces",
        ))
    }

    fn finalize_context(&self) -> Result<IsolatedCommandFinalizeContext, WorkspaceApiError> {
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "isolated adapter cannot provide command finalize context",
        ))
    }

    fn finalize_isolated_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        let _ = request;
        Err(WorkspaceApiError::new(
            "unsupported_command_workspace_adapter",
            "isolated adapter cannot finalize command workspaces",
        ))
    }
}
