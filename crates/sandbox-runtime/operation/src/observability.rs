use std::path::PathBuf;

use crate::namespace_execution::RuntimeNamespaceExecutionSnapshot;
use crate::services::WorkloadCgroupLimits;
use crate::workspace_crate::{NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::{FinalizationState, FinalizePolicy};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeObservabilitySnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub active_namespace_executions: Vec<RuntimeNamespaceExecutionSnapshot>,
    pub ownership: RuntimeOwnershipSnapshot,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOwnershipTopologySnapshot {
    pub workspaces: Vec<RuntimeTopologyWorkspaceSnapshot>,
    pub active_command_count: usize,
    pub active_layer_lease_count: usize,
    pub ownership: RuntimeOwnershipSnapshot,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOwnershipSnapshot {
    pub namespace_fd_count: Option<usize>,
    pub control_fd_count: Option<usize>,
    pub active_scratch_directories: Option<usize>,
    pub persisted_workspace_handles: Option<usize>,
    pub exited_unreaped_holders: Option<usize>,
    pub scratch_layout_version: Option<u8>,
    pub scratch_route: Option<String>,
    pub active_execution_scratch_leases: Option<usize>,
    pub retained_terminal_records: Option<usize>,
    pub open_transcript_descriptors: Option<usize>,
    pub live_execution_scratch_bytes: Option<u64>,
    pub high_water_execution_scratch_bytes: Option<u64>,
    pub teardown_join_total: Option<u64>,
    pub teardown_deadline_total: Option<u64>,
    pub legacy_entries_scanned: Option<usize>,
    pub legacy_entries_deleted: Option<usize>,
    pub legacy_entries_skipped_active: Option<usize>,
    pub legacy_entries_skipped_recent: Option<usize>,
    pub legacy_entries_skipped_unsafe: Option<usize>,
    pub scratch_cleanup_state: Option<String>,
    pub scratch_quiescent: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceSnapshot {
    pub workspace_id: WorkspaceSessionId,
    pub holder_pid: i32,
    pub holder_live: bool,
    pub network: NetworkProfile,
    pub finalize_policy: FinalizePolicy,
    pub finalization_state: FinalizationState,
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
    pub applied_cgroup_limits: Option<WorkloadCgroupLimits>,
    pub workload_cgroup_state: String,
    pub workload_cgroup_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTopologyWorkspaceSnapshot {
    pub workspace_id: WorkspaceSessionId,
    pub holder_pid: i32,
    pub holder_live: bool,
    pub cgroup_path: Option<PathBuf>,
    pub applied_cgroup_limits: Option<WorkloadCgroupLimits>,
    pub workload_cgroup_state: String,
    pub workload_cgroup_reason: Option<String>,
}
