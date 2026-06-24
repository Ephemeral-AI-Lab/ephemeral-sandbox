//! Workspace profile manager.
//!
//! The manager owns admission policy, persistence, and the lifecycle
//! modules own profile-specific setup, shared holder, overlay, teardown, and
//! persistence behavior.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::isolated_setup::IsolatedNetwork;
use crate::namespace::NamespaceRuntime;
pub use crate::profile::{
    WorkspaceModeFds, WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeSnapshot,
};

pub use crate::lifecycle::remount::{RemountOverlayResult, RemountProbe, WorkspaceRemountState};
pub use crate::lifecycle::ExitOutcome;

pub(crate) const PERSISTED_HANDLES_SCHEMA_VERSION: u32 = 1;
const HOST_BUDGET_FALLBACK_BYTES: u64 = 1_u64 << 62;
const KIB_BYTES: u64 = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceCaps {
    pub upperdir_bytes: u64,
    pub memavail_fraction: f64,
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

impl Default for ResourceCaps {
    fn default() -> Self {
        Self {
            upperdir_bytes: 1_073_741_824,
            memavail_fraction: 0.5,
            setup_timeout_s: 30.0,
            exit_grace_s: 0.25,
            rfc1918_egress: Rfc1918Egress::Allow,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceModeError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("workspace session is not open")]
    NotOpen,

    #[error("host RAM gate refuses new workspace session")]
    HostRamPressure {
        required_bytes: u64,
        budget_bytes: u64,
    },

    #[error("setup failed at step {step}")]
    SetupFailed { step: String },

    #[error("isolated network unavailable: {0}")]
    NetworkUnavailable(String),
}

impl WorkspaceModeError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) => "invalid_argument",
            Self::NotOpen => "not_open",
            Self::HostRamPressure { .. } => "host_ram_pressure",
            Self::SetupFailed { .. } | Self::NetworkUnavailable(_) => "setup_failed",
        }
    }
}

pub struct WorkspaceModeManager {
    pub(crate) workspace_root: String,
    pub(crate) caps: ResourceCaps,
    pub(crate) runtime: NamespaceRuntime,
    pub(crate) network: IsolatedNetwork,
    pub(crate) scratch_root: PathBuf,
    pub(crate) handles: HashMap<WorkspaceModeId, WorkspaceModeHandle>,
}

impl WorkspaceModeManager {
    #[must_use]
    pub fn new(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_root: PathBuf,
    ) -> Self {
        let runtime = NamespaceRuntime::new(caps.setup_timeout_s);
        Self::with_runtime(workspace_root, caps, scratch_root, runtime)
    }

    pub(crate) fn with_runtime(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_root: PathBuf,
        runtime: NamespaceRuntime,
    ) -> Self {
        let network = IsolatedNetwork::new(caps.rfc1918_egress);
        Self {
            workspace_root: workspace_root.into(),
            caps,
            runtime,
            network,
            scratch_root,
            handles: HashMap::new(),
        }
    }

    pub(crate) fn check_host_capacity(&self) -> Result<(), WorkspaceModeError> {
        check_host_capacity_against_budget(
            self.handles.len(),
            self.caps.upperdir_bytes,
            host_capacity_budget_bytes(self.caps.memavail_fraction),
        )
    }

    pub(crate) fn workspace_session_root(&self, workspace_id: &WorkspaceModeId) -> PathBuf {
        self.scratch_root.join("sessions").join(&workspace_id.0)
    }
}

pub(crate) fn validate_workspace_root(workspace_root: &str) -> Result<(), WorkspaceModeError> {
    let workspace_root = workspace_root.trim();
    if workspace_root.is_empty() {
        return Err(WorkspaceModeError::InvalidArgument(
            "workspace_root is required".to_owned(),
        ));
    }
    if !Path::new(workspace_root).is_absolute() {
        return Err(WorkspaceModeError::InvalidArgument(format!(
            "workspace_root must be absolute: {workspace_root}"
        )));
    }
    Ok(())
}

fn check_host_capacity_against_budget(
    open_handles: usize,
    upperdir_bytes: u64,
    budget_bytes: u64,
) -> Result<(), WorkspaceModeError> {
    let required_bytes = required_host_capacity_bytes(open_handles, upperdir_bytes);
    if required_bytes > budget_bytes {
        return Err(WorkspaceModeError::HostRamPressure {
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
