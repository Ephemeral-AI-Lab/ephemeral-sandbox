#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CommandBinding {
    pub caller_id: String,
    pub workspace_handle_id: String,
    pub layer_stack_root: PathBuf,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub workspace_root: PathBuf,
    pub scratch_dir: PathBuf,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub ns_fds: HashMap<String, i32>,
    pub cgroup_path: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
mod ops;
mod outcome;
#[cfg(target_os = "linux")]
mod prepare;
mod registry;
pub mod runtime;
#[cfg(target_os = "linux")]
mod settle;

#[cfg(target_os = "linux")]
pub use ops::{CommandOps, ExecTarget};
pub use outcome::{ChangedPathKinds, WorkspaceConflict, WorkspaceTimings};
pub use runtime::{
    active_command_sessions_for_caller, cancel_all_command_sessions,
    cleanup_command_sessions_for_caller, command_ops, command_session_config,
    command_session_scratch_root, configure_command_sessions,
};
