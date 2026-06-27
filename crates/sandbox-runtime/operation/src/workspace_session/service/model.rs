use std::path::PathBuf;

use crate::workspace_crate::{BaseRevision, WorkspaceHandle, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSessionHandler {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub cgroup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceSession {
    pub workspace_session_id: WorkspaceSessionId,
    pub handle: WorkspaceHandle,
    pub cgroup_path: Option<PathBuf>,
}

impl WorkspaceSession {
    pub(crate) fn from_handle(handle: WorkspaceHandle, cgroup_path: Option<PathBuf>) -> Self {
        Self {
            workspace_session_id: handle.id.clone(),
            handle,
            cgroup_path,
        }
    }

    pub(crate) fn handler(&self) -> WorkspaceSessionHandler {
        WorkspaceSessionHandler {
            workspace_session_id: self.workspace_session_id.clone(),
            handle: self.handle.clone(),
            cgroup_path: self.cgroup_path.clone(),
        }
    }

    pub(crate) fn active_handle(&self) -> Result<WorkspaceHandle, WorkspaceSessionError> {
        Ok(self.handle.clone())
    }

    pub(crate) fn refresh_after_capture(&mut self, base_revision: BaseRevision) {
        self.handle.snapshot.manifest_version = base_revision.version;
        self.handle.snapshot.root_hash = base_revision.root_hash;
    }
}
