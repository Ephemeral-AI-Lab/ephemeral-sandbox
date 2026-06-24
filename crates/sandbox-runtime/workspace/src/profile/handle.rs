use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::isolated_setup::VethAllocation;
use crate::model::WorkspaceProfile;
use crate::overlay::dirs::OverlayDirs;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceModeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceModeSnapshot {
    pub lease_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub base_manifest: sandbox_runtime_layerstack::Manifest,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceModeHandle {
    pub workspace_id: WorkspaceModeId,
    pub profile: WorkspaceProfile,
    pub lease_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub base_manifest: sandbox_runtime_layerstack::Manifest,
    pub workspace_root: String,
    pub dirs: OverlayDirs,
    pub layer_paths: Vec<PathBuf>,
    pub ns_fds: WorkspaceModeFds,
    pub holder_pid: i32,
    pub readiness_fd: i32,
    pub control_fd: i32,
    pub veth: Option<VethAllocation>,
    pub created_at: f64,
    pub last_activity: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceModeFds {
    pub user: Option<i32>,
    pub mnt: Option<i32>,
    pub pid: Option<i32>,
    pub net: Option<i32>,
}

impl WorkspaceModeFds {
    pub(crate) fn len(self) -> usize {
        self.values().count()
    }

    pub(crate) fn is_empty(self) -> bool {
        self.user.is_none() && self.mnt.is_none() && self.pid.is_none() && self.net.is_none()
    }

    pub(crate) fn values(self) -> impl Iterator<Item = i32> {
        [self.user, self.mnt, self.pid, self.net]
            .into_iter()
            .flatten()
    }
}
