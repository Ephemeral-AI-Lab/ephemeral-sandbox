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

pub use crate::lifecycle::ExitOutcome;

pub(crate) const PERSISTED_HANDLES_SCHEMA_VERSION: u32 = 1;

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
