use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::isolated_network_setup::{IsolatedNetwork, VethAllocation};
use crate::isolated_workspace::caps::ResourceCaps;
use crate::isolated_workspace::error::IsolatedError;
use crate::lifecycle::monotonic_seconds;
use crate::namespace::NamespaceRuntime;
use crate::overlay::dirs::OverlayDirs;

#[cfg(test)]
#[path = "../../../tests/unit/isolated_workspace_sessions.rs"]
mod tests;

pub use crate::lifecycle::remount::WorkspaceRemountState;
pub use crate::lifecycle::ExitOutcome;

const HOST_BUDGET_FALLBACK_BYTES: u64 = 1_u64 << 62;
const KIB_BYTES: u64 = 1_024;
const OWNED_SCRATCH_DIR: &str = "eos-isolated";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IsolatedWorkspaceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedSnapshot {
    pub lease_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsConfiguration {
    pub fallback_applied: bool,
    pub previous_first_nameserver: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceHandle {
    pub workspace_id: IsolatedWorkspaceId,
    pub caller_id: String,
    pub lease_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub workspace_root: String,
    pub dirs: OverlayDirs,
    pub layer_paths: Vec<PathBuf>,
    pub ns_fds: HashMap<String, i32>,
    pub holder_pid: i32,
    pub readiness_fd: i32,
    pub control_fd: i32,
    pub veth: Option<VethAllocation>,
    pub cgroup_path: Option<PathBuf>,
    pub dns_configuration: DnsConfiguration,
    pub remount_state: WorkspaceRemountState,
    pub created_at: f64,
    pub last_activity: f64,
}

pub struct IsolatedManager {
    pub(crate) caps: ResourceCaps,
    pub(crate) runtime: NamespaceRuntime,
    pub(crate) network: IsolatedNetwork,
    pub(crate) scratch_root: PathBuf,
    pub(crate) handles: HashMap<IsolatedWorkspaceId, WorkspaceHandle>,
    pub(crate) by_caller: HashMap<String, IsolatedWorkspaceId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanCleanupReport {
    pub orphan_lease_ids: Vec<String>,
    pub cleanup_error: Option<String>,
}

impl IsolatedManager {
    #[must_use]
    pub fn with_scratch_root(caps: ResourceCaps, scratch_root: PathBuf) -> Self {
        Self::with_runtime(caps, scratch_root, NamespaceRuntime::from_env())
    }

    #[must_use]
    pub fn stubbed(caps: ResourceCaps, scratch_root: PathBuf) -> Self {
        Self::with_runtime(caps, scratch_root, NamespaceRuntime::stubbed())
    }

    pub(crate) fn with_runtime(
        caps: ResourceCaps,
        scratch_root: PathBuf,
        runtime: NamespaceRuntime,
    ) -> Self {
        let network = IsolatedNetwork::new(caps.rfc1918_egress);
        Self {
            caps,
            runtime,
            network,
            scratch_root,
            handles: HashMap::new(),
            by_caller: HashMap::new(),
        }
    }

    pub(crate) fn check_host_capacity(&self) -> Result<(), IsolatedError> {
        check_host_capacity_against_budget(
            self.handles.len(),
            self.caps.upperdir_bytes,
            host_capacity_budget_bytes(self.caps.memavail_fraction),
        )
    }

    pub fn initialize_report(&mut self) -> Result<OrphanCleanupReport, IsolatedError> {
        if !self.caps.enabled {
            return Err(IsolatedError::FeatureDisabled);
        }
        self.network.initialize()?;
        std::fs::create_dir_all(&self.scratch_root).map_err(|err| IsolatedError::SetupFailed {
            step: format!("scratch_root: {err}"),
        })?;
        self.reap_persisted_orphans()
    }

    #[must_use]
    pub fn get_handle(&self, caller_id: &str) -> Option<WorkspaceHandle> {
        self.by_caller
            .get(caller_id)
            .and_then(|workspace_id| self.handles.get(workspace_id))
            .cloned()
    }

    #[must_use]
    pub fn list_open_callers(&self) -> Vec<String> {
        self.by_caller.keys().cloned().collect()
    }

    pub fn touch(&mut self, caller_id: &str) {
        if let Some(handle) = self
            .by_caller
            .get(caller_id)
            .and_then(|workspace_id| self.handles.get_mut(workspace_id))
        {
            handle.last_activity = monotonic_seconds();
        }
    }

    pub fn reap_orphan_resources(&mut self) -> Option<String> {
        self.reap_named_orphans()
    }

    pub(crate) fn owned_scratch_root(&self) -> PathBuf {
        self.scratch_root.join(OWNED_SCRATCH_DIR)
    }
}

fn check_host_capacity_against_budget(
    open_handles: usize,
    upperdir_bytes: u64,
    budget_bytes: u64,
) -> Result<(), IsolatedError> {
    let required_bytes = required_host_capacity_bytes(open_handles, upperdir_bytes);
    if required_bytes > budget_bytes {
        return Err(IsolatedError::HostRamPressure {
            required_bytes,
            budget_bytes,
        });
    }
    Ok(())
}

fn required_host_capacity_bytes(open_handles: usize, upperdir_bytes: u64) -> u64 {
    u64::try_from(open_handles)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .saturating_mul(upperdir_bytes)
}

fn host_capacity_budget_bytes(memavail_fraction: f64) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| parse_memavailable_kib(&meminfo))
        .map_or(HOST_BUDGET_FALLBACK_BYTES, |memavailable_kib| {
            host_capacity_budget_bytes_from_memavailable_kib(memavailable_kib, memavail_fraction)
        })
}

fn parse_memavailable_kib(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("MemAvailable:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn host_capacity_budget_bytes_from_memavailable_kib(
    memavailable_kib: u64,
    memavail_fraction: f64,
) -> u64 {
    (memavailable_kib.saturating_mul(KIB_BYTES) as f64 * memavail_fraction).floor() as u64
}
