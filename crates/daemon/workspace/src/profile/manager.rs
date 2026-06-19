//! Caller-keyed workspace profile manager.
//!
//! The manager owns admission policy, quotas, caller indexing, persistence, and
//! orphan cleanup. Profile-specific environment setup lives in
//! `profile::host_compatible` and `profile::isolated`; shared holder, overlay,
//! cgroup, teardown, and recovery lifecycle lives in `profile::common` and the
//! lifecycle modules.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::isolated_network_setup::IsolatedNetwork;
use crate::lifecycle::monotonic_seconds;
use crate::namespace::NamespaceRuntime;
pub use crate::profile::{
    DnsConfiguration, WorkspaceModeContext, WorkspaceModeHandle, WorkspaceModeId,
    WorkspaceModeSnapshot,
};

#[cfg(test)]
#[path = "../../tests/unit/isolated_network_sessions.rs"]
mod tests;

pub use crate::lifecycle::remount::{
    RemountOverlayReport, RemountProbe, RemountedWorkspace, WorkspaceRemountState,
};
pub use crate::lifecycle::ExitOutcome;

pub(crate) const PERSISTED_HANDLES_SCHEMA_VERSION: u32 = 1;

const DEFAULT_EOS_WORKSPACE_ROOT: &str = "/testbed";
const HOST_BUDGET_FALLBACK_BYTES: u64 = 1_u64 << 62;
const KIB_BYTES: u64 = 1_024;
const OWNED_SCRATCH_DIR: &str = "eos-isolated";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceCaps {
    pub enabled: bool,
    pub ttl_s: f64,
    pub total_cap: u32,
    pub upperdir_bytes: u64,
    pub memavail_fraction: f64,
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
    pub fallback_dns: String,
    pub eos_workspace_root: String,
}

impl Default for ResourceCaps {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_s: 1800.0,
            total_cap: 5,
            upperdir_bytes: 1_073_741_824,
            memavail_fraction: 0.5,
            setup_timeout_s: 30.0,
            exit_grace_s: 0.25,
            rfc1918_egress: Rfc1918Egress::Allow,
            fallback_dns: "1.1.1.1".to_owned(),
            eos_workspace_root: DEFAULT_EOS_WORKSPACE_ROOT.to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IsolatedNetworkError {
    #[error("isolated networks are disabled")]
    FeatureDisabled,

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("agent already has an open isolated network")]
    AlreadyOpen { created_at: f64, last_activity: f64 },

    #[error("agent has no open isolated network")]
    NotOpen,

    #[error("global isolated network cap reached")]
    QuotaExceeded { total_cap: u32 },

    #[error("host RAM gate refuses new isolated network")]
    HostRamPressure {
        required_bytes: u64,
        budget_bytes: u64,
    },

    #[error("setup failed at step {step}")]
    SetupFailed { step: String },

    #[error("isolated network unavailable: {0}")]
    NetworkUnavailable(String),
}

impl IsolatedNetworkError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::FeatureDisabled => "feature_disabled",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::AlreadyOpen { .. } => "already_open",
            Self::NotOpen => "not_open",
            Self::QuotaExceeded { .. } => "quota_exceeded",
            Self::HostRamPressure { .. } => "host_ram_pressure",
            Self::SetupFailed { .. } | Self::NetworkUnavailable(_) => "setup_failed",
        }
    }
}

pub struct WorkspaceModeManager {
    pub(crate) caps: ResourceCaps,
    pub(crate) runtime: NamespaceRuntime,
    pub(crate) network: IsolatedNetwork,
    pub(crate) scratch_root: PathBuf,
    pub(crate) handles: HashMap<WorkspaceModeId, WorkspaceModeHandle>,
    pub(crate) by_caller: HashMap<String, WorkspaceModeId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanCleanupReport {
    pub orphan_lease_ids: Vec<String>,
    pub cleanup_error: Option<String>,
}

impl WorkspaceModeManager {
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

    pub(crate) fn check_host_capacity(&self) -> Result<(), IsolatedNetworkError> {
        check_host_capacity_against_budget(
            self.handles.len(),
            self.caps.upperdir_bytes,
            host_capacity_budget_bytes(self.caps.memavail_fraction),
        )
    }

    pub fn initialize_report(&mut self) -> Result<OrphanCleanupReport, IsolatedNetworkError> {
        if !self.caps.enabled {
            return Err(IsolatedNetworkError::FeatureDisabled);
        }
        std::fs::create_dir_all(&self.scratch_root).map_err(|err| {
            IsolatedNetworkError::SetupFailed {
                step: format!("scratch_root: {err}"),
            }
        })?;
        self.reap_persisted_orphans()
    }

    #[must_use]
    pub fn get_handle(&self, caller_id: &str) -> Option<WorkspaceModeHandle> {
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
) -> Result<(), IsolatedNetworkError> {
    let required_bytes = required_host_capacity_bytes(open_handles, upperdir_bytes);
    if required_bytes > budget_bytes {
        return Err(IsolatedNetworkError::HostRamPressure {
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
