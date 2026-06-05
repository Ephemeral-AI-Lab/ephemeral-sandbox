use std::collections::HashMap;
use std::path::PathBuf;

use eos_workspace_api::{WorkspaceApiError, WorkspaceTimings};
use serde_json::Value;

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
    pub caller_id: String,
    pub workspace_handle_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub upperdir: PathBuf,
    pub base_timings: WorkspaceTimings,
}

/// Daemon-supplied port for isolated command-session prepare/finalize policy.
///
/// This port exposes no publish capability. It exists so isolated command
/// workspace policy compiles against `CommandWorkspacePolicy` while daemon PTY,
/// child process, registry, and reaper control remain in `eos-daemon`.
pub trait IsolatedCommandSessionPort {
    fn command_session_started(&self, command_session_id: &str, caller_id: &str) {
        let _ = (command_session_id, caller_id);
    }

    fn command_session_finished(&self, command_session_id: &str, caller_id: &str, status: &str) {
        let _ = (command_session_id, caller_id, status);
    }

    fn prepare_context(&self) -> Result<IsolatedCommandPrepareContext, WorkspaceApiError>;

    fn finalize_context(&self) -> Result<IsolatedCommandFinalizeContext, WorkspaceApiError>;

    fn record_command_audit(&self, payload: Value);
}
