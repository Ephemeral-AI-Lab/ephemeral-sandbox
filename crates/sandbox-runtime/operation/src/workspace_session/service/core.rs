use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use sandbox_observability::Observer;

use crate::workspace_crate::{WorkspaceRuntimeService, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionError;

use super::model::WorkspaceSession;

pub struct WorkspaceSessionService {
    sessions: Mutex<HashMap<WorkspaceSessionId, WorkspaceSession>>,
    workspace: Arc<WorkspaceRuntimeService>,
    cgroup_root: Option<PathBuf>,
    obs: Observer,
}

impl WorkspaceSessionService {
    #[must_use]
    pub fn new(workspace: Arc<WorkspaceRuntimeService>, obs: Observer) -> Self {
        Self::with_cgroup_root(workspace, None, obs)
    }

    #[must_use]
    pub fn with_cgroup_root(
        workspace: Arc<WorkspaceRuntimeService>,
        cgroup_root: Option<PathBuf>,
        obs: Observer,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            workspace,
            cgroup_root,
            obs,
        }
    }

    #[must_use]
    pub(crate) fn workspace(&self) -> &Arc<WorkspaceRuntimeService> {
        &self.workspace
    }

    #[must_use]
    pub(crate) fn obs(&self) -> &Observer {
        &self.obs
    }

    /// Create the leaf workspace cgroup `R/workspace-<wsid>` when a delegated
    /// cgroup root is configured. Best-effort: directory creation never blocks
    /// session creation, and an unconfigured root yields `None`.
    pub(crate) fn prepare_workspace_cgroup(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Option<PathBuf> {
        let path = self
            .cgroup_root
            .as_ref()?
            .join(format!("workspace-{}", workspace_session_id.0));
        let _ = std::fs::create_dir_all(&path);
        Some(path)
    }

    pub(crate) fn lock_sessions(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<WorkspaceSessionId, WorkspaceSession>>, WorkspaceSessionError>
    {
        self.sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)
    }
}
