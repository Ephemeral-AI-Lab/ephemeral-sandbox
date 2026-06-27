//! Workspace manager.
//!
//! The manager owns admission policy, persistence, and the lifecycle
//! modules own network-mode-specific setup, shared holder, overlay, teardown,
//! and persistence behavior.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sandbox_observability::Observer;
use serde::Deserialize;

use crate::isolated_network_setup::IsolatedNetwork;
use crate::model::WorkspaceSessionId;
use crate::namespace::NamespaceRuntime;
pub use crate::session::{HolderNsFds, MountedWorkspace};

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
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

impl Default for ResourceCaps {
    fn default() -> Self {
        Self {
            setup_timeout_s: 30.0,
            exit_grace_s: 0.25,
            rfc1918_egress: Rfc1918Egress::Allow,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceManagerError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("workspace session is not open")]
    NotOpen,

    #[error("setup failed at step {step}")]
    SetupFailed { step: String },

    #[error("isolated network unavailable: {0}")]
    NetworkUnavailable(String),
}

pub struct WorkspaceManager {
    pub(crate) workspace_root: String,
    pub(crate) caps: ResourceCaps,
    pub(crate) runtime: NamespaceRuntime,
    pub(crate) network: IsolatedNetwork,
    pub(crate) scratch_root: PathBuf,
    pub(crate) handles: HashMap<WorkspaceSessionId, MountedWorkspace>,
}

impl WorkspaceManager {
    #[must_use]
    pub fn new(
        workspace_root: impl Into<String>,
        caps: ResourceCaps,
        scratch_root: PathBuf,
        obs: Observer,
    ) -> Self {
        let runtime = NamespaceRuntime::new(caps.setup_timeout_s, obs);
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

    pub(crate) fn workspace_session_root(&self, workspace_id: &WorkspaceSessionId) -> PathBuf {
        self.scratch_root.join(&workspace_id.0)
    }
}

pub(crate) fn validate_workspace_root(workspace_root: &str) -> Result<(), WorkspaceManagerError> {
    let workspace_root = workspace_root.trim();
    if workspace_root.is_empty() {
        return Err(WorkspaceManagerError::InvalidArgument(
            "workspace_root is required".to_owned(),
        ));
    }
    if !Path::new(workspace_root).is_absolute() {
        return Err(WorkspaceManagerError::InvalidArgument(format!(
            "workspace_root must be absolute: {workspace_root}"
        )));
    }
    Ok(())
}
