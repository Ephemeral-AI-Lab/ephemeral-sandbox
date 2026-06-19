use crate::namespace::NamespacePlan;
use crate::profile::common::ProfileHooks;

use crate::model::NetworkMode;

// Temporary compatibility exports for legacy WorkspaceRuntime and
// operation::command host paths. Removal criteria: Milestone 7 daemon dispatch
// migration routes callers through profile-neutral workspace/session handles.
#[doc(hidden)]
pub use crate::profile::host_workspace::{
    HostNamespaceWorkspaceRequest, HostWorkspace, HostWorkspaceError, WorkspaceNamespaceFds,
};

#[derive(Debug, Default)]
pub(crate) struct HostCompatibleProfile;

impl ProfileHooks for HostCompatibleProfile {
    fn kind(&self) -> NetworkMode {
        NetworkMode::Host
    }

    fn namespace_plan(&self) -> NamespacePlan {
        NamespacePlan::host_workspace()
    }
}
