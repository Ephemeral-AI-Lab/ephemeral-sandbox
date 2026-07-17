use std::path::PathBuf;

use crate::namespace_execution::RuntimeNamespaceExecutionSnapshot;
use crate::workspace_crate::{NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::FinalizePolicy;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeObservabilitySnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub active_namespace_executions: Vec<RuntimeNamespaceExecutionSnapshot>,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceSnapshot {
    pub workspace_id: WorkspaceSessionId,
    pub holder_pid: i32,
    pub network: NetworkProfile,
    pub finalize_policy: FinalizePolicy,
    pub workspace_root: PathBuf,
    pub upperdir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub namespace_fd_count: Option<usize>,
    pub base_root_hash: Option<String>,
    pub layer_count: Option<usize>,
    /// Mounted layer ids, base → newest. The per-session layerstack view joins
    /// these across workspaces to derive layer sharing.
    pub layer_ids: Vec<String>,
    pub cgroup_path: Option<PathBuf>,
}
