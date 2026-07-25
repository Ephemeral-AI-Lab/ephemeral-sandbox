use crate::isolated_network_setup::VethAllocation;
use crate::model::{LayerStackSnapshotRef, NetworkProfile, WorkspaceSessionId};
use crate::namespace::holder::HolderRegistration;
use crate::overlay::dirs::OverlayDirs;

#[derive(Debug, Clone)]
pub struct MountedWorkspace {
    pub workspace_id: WorkspaceSessionId,
    pub network: NetworkProfile,
    pub snapshot: LayerStackSnapshotRef,
    /// Exact native generation selected and durably leased before this
    /// workspace's private scratch or namespace was created.
    #[doc(hidden)]
    pub candidate_admission:
        Option<sandbox_runtime_layerstack::service::CandidateGenerationAdmission>,
    pub workspace_root: String,
    pub dirs: OverlayDirs,
    pub ns_fds: HolderNsFds,
    pub holder_pid: i32,
    #[doc(hidden)]
    pub holder_registration: HolderRegistration,
    pub readiness_fd: i32,
    pub control_fd: i32,
    pub veth: Option<VethAllocation>,
    pub created_at: f64,
    pub last_activity: f64,
    /// The session's second lease when one exists: the OLD lease after an
    /// EBUSY-parked switch, or the NEW lease on a faulty remount. In-memory
    /// only — never persisted — and released by the ordinary destroy path.
    pub parked_lease_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HolderNsFds {
    pub user: Option<i32>,
    pub mnt: Option<i32>,
    pub pid: Option<i32>,
    pub net: Option<i32>,
}

impl HolderNsFds {
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
